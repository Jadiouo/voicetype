//! 編譯 SenseVoice 的 C shim 並連結引擎。
//!
//! 刻意**不**在這裡驅動 SenseVoice.cpp 的 CMake 建置: 那是一個含 ggml
//! 的大型專案, 首次建置數分鐘, 綁進 `cargo build` 會讓每個開發循環都
//! 有機會踩到它。改成要求先建好 third_party (README 有步驟), 這裡只
//! 編 shim 並連結。缺了就給明確指示而不是默默失敗。

use std::path::{Path, PathBuf};

fn main() {
    link_opencc();
    if std::env::var_os("CARGO_FEATURE_SHERPA_NANO").is_some() {
        link_nano();
    }

    // 引擎放在 feature 後面: 沒有 third_party 的環境 (CI、只改 IPC 的
    // 開發循環) 仍然要能 `cargo build` 與跑單元測試。
    if std::env::var("CARGO_FEATURE_SENSEVOICE").is_err() {
        return;
    }

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.parent().expect("workspace root");
    let sv = root.join("third_party").join("SenseVoice.cpp");
    println!("cargo:rerun-if-changed=shim/sensevoice_shim.cpp");
    println!("cargo:rerun-if-changed=shim/sensevoice_shim.h");

    // `build-static` 優先: 評測腳本用的 `build` 是動態連結的 (ggml 是 .so),
    // 連進 daemon 會帶來 rpath 問題。兩個目錄並存讓兩邊各取所需。
    let lib_dir = ["build-static", "build"]
        .iter()
        .map(|d| sv.join(d).join("lib"))
        .find(|d| d.join("libggml-base.a").exists())
        .unwrap_or_else(|| sv.join("build-static").join("lib"));

    if !lib_dir.join("libsense-voice-core.a").exists() {
        panic!(
            "\n\n找不到 SenseVoice.cpp 的靜態庫:\n  {}\n\n\
             請先建置引擎 (全靜態, 避免 rpath 問題):\n\n  \
             cmake -S third_party/SenseVoice.cpp -B third_party/SenseVoice.cpp/build-static \\\n    \
             -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=OFF -DBUILD_SHARED_LIBS=OFF\n  \
             cmake --build third_party/SenseVoice.cpp/build-static -j\n\n\
             GGML_CUDA=OFF 不是省略而是刻意的 —— 見 SDD §2/C2。\n",
            lib_dir.display()
        );
    }

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("shim/sensevoice_shim.cpp")
        .include("shim")
        .include(sv.join("sense-voice").join("csrc"))
        // ggml 是 SenseVoice.cpp 的 vendored 子專案, 不在頂層。
        .include(
            sv.join("sense-voice")
                .join("csrc")
                .join("third-party")
                .join("ggml")
                .join("include"),
        )
        .warnings(false)
        .compile("sensevoice_shim");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=sense-voice-core");

    // ggml 的庫名隨版本而異, 且靜態建置下會拆成數個。逐一探測比寫死
    // 清單穩健 —— 上游改名時錯誤會落在這裡, 而不是連結階段的一堆
    // undefined reference。
    for name in ["ggml", "ggml-base", "ggml-cpu"] {
        if lib_dir.join(format!("lib{name}.a")).exists() {
            println!("cargo:rustc-link-lib=static={name}");
        }
    }

    link_extra(&lib_dir);

    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rustc-link-lib=gomp"); // ggml-cpu 用 OpenMP
}

fn link_nano() {
    println!("cargo:rerun-if-env-changed=VOICETYPE_SHERPA_NATIVE_ROOT");
    println!("cargo:rerun-if-env-changed=VOICETYPE_SHERPA_CAPI_SHA256");
    println!("cargo:rerun-if-changed=shim/nano_shim.cpp");
    println!("cargo:rerun-if-changed=shim/nano_shim.h");
    let root = PathBuf::from(std::env::var_os("VOICETYPE_SHERPA_NATIVE_ROOT")
        .expect("sherpa-nano requires explicit VOICETYPE_SHERPA_NATIVE_ROOT (pinned 1.13.8 CPU integrity build)"))
        .canonicalize().expect("native Nano artifact root does not exist");
    assert!(!root.to_string_lossy().contains([':', ',', '\n']), "native artifact path cannot contain colon/comma/newline");
    let include = root.join("include");
    let lib = root.join("lib");
    // An explicit release-builder pin is necessary because compilers produce
    // different C API bytes. Only the isolated desktop build can use it. The
    // reviewed source-build pipeline verifies inputs and passes its exact output
    // digest; a runtime/installer sidecar is never allowed to choose this value.
    let capi_sha = std::env::var("VOICETYPE_SHERPA_CAPI_SHA256").unwrap_or_else(|_| {
        "72408cc5f2407eb0ba46cd381614229107f225b8ccc4149e2f5e4b09957834dd".into()
    });
    if std::env::var_os("VOICETYPE_SHERPA_CAPI_SHA256").is_some() {
        assert!(std::env::var_os("CARGO_FEATURE_RELOCATABLE_RUNTIME").is_some(),
            "a release-builder C API pin is allowed only with relocatable-runtime");
        assert!(capi_sha.len() == 64 && capi_sha.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "invalid release-builder C API SHA256");
    }
    for (path, expected) in [
        (include.join("sherpa-onnx/c-api/c-api.h"), "2a1b95084be8fd1deb3228fcad2fd3f7f0258b64582f7402281ec174c7b7f4ce"),
        (lib.join("libsherpa-onnx-c-api.so"), capi_sha.as_str()),
        (lib.join("libonnxruntime.so"), "4b3607aebd1784b26b6f9b20e4bd974c7ab8287043e4d095cb7d2cb40b5e566e"),
    ] {
        assert!(path.is_file(), "missing native Nano dependency: {}", path.display());
        let digest = std::process::Command::new("sha256sum").arg("--").arg(&path)
            .output().expect("native Nano artifact verification requires sha256sum");
        assert!(digest.status.success() && String::from_utf8_lossy(&digest.stdout).split_whitespace().next() == Some(expected),
            "native Nano artifact hash differs from pinned 1.13.8 CPU integrity build: {}", path.display());
        println!("cargo:rerun-if-changed={}", path.display());
    }
    cc::Build::new().cpp(true).std("c++17").file("shim/nano_shim.cpp")
        .include("shim").include(include).compile("nano_shim");
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=dylib=sherpa-onnx-c-api");
    println!("cargo:rustc-link-lib=stdc++");
    // The desktop bundle preserves the pinned pair in ../lib. The development
    // profile keeps its explicit artifact path; neither resolves arbitrary
    // libraries through LD_LIBRARY_PATH supplied by the application.
    if std::env::var_os("CARGO_FEATURE_RELOCATABLE_RUNTIME").is_some() {
        assert_eq!(std::env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("linux"),
            "relocatable-runtime currently supports Linux only");
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags,-rpath,$ORIGIN/../lib");
    } else {
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags,-rpath,{}", lib.display());
    }
}

/// 連結 OpenCC (SDD §4.6 ② 的繁化)。
///
/// 不透過 feature: 輸出繁體是 §1.1 的目標之一, 不是選配。而 OpenCC 是
/// 一行 apt 就有的系統套件, 與 third_party 那種要手動 cmake 的相依性
/// 不同, 不值得為它多一個 feature 維度。
///
/// 優先用 pkg-config。找不到時退回掃描標準 libdir —— Debian/Ubuntu 把
/// `libopencc.so` 這個開發用的 symlink 放在 `libopencc-dev` 裡, 只裝了
/// runtime (`libopencc1.1`) 的系統會有 `.so.1.1` 卻沒有 `.so`,
/// `-lopencc` 因此找不到東西。這種情形下用 `-l:` 指名實際檔名。
fn link_opencc() {
    println!("cargo:rerun-if-changed=build.rs");

    if pkg_config_ok() {
        return;
    }

    for dir in [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
        "/usr/local/lib",
    ] {
        if Path::new(dir).join("libopencc.so").exists() {
            println!("cargo:rustc-link-search=native={dir}");
            println!("cargo:rustc-link-lib=dylib=opencc");
            return;
        }
        if let Some(soname) = newest_soname(dir) {
            // 連結器要的是 `libopencc.so` 這個名字, 而 runtime 包沒有它。
            // 在 OUT_DIR 補一個 symlink 指向實際檔案 —— 比讓 cargo 傳
            // `-l:libopencc.so.1.1.7` 穩健 (那個語法要靠 verbatim
            // modifier, 而且把版本號寫死進連結指令)。
            //
            // 執行期不受影響: loader 找的是 ELF 裡的 SONAME
            // (libopencc.so.1.1), ldconfig 早就知道它在哪。
            let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
            let link = out.join("libopencc.so");
            let _ = std::fs::remove_file(&link);
            if std::os::unix::fs::symlink(Path::new(dir).join(&soname), &link).is_ok() {
                println!("cargo:rustc-link-search=native={}", out.display());
                println!("cargo:rustc-link-lib=dylib=opencc");
                return;
            }
        }
    }

    panic!(
        "\n\n找不到 OpenCC。繁化是 SDD §1.1 的目標之一, 不是選配 ——\n\
         沒有它輸出會是簡體中文。\n\n  \
         Debian/Ubuntu:  sudo apt install libopencc-dev\n  \
         Fedora:         sudo dnf install opencc-devel\n  \
         Arch:           sudo pacman -S opencc\n"
    );
}

fn pkg_config_ok() -> bool {
    let Ok(out) = std::process::Command::new("pkg-config")
        .args(["--libs", "opencc"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    for flag in String::from_utf8_lossy(&out.stdout).split_whitespace() {
        if let Some(dir) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={dir}");
        } else if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }
    true
}

/// 挑 `libopencc.so.*` 裡版本最高的一個。多版本並存時舊的通常是
/// 相容性遺留, 連到新的比較合理。
fn newest_soname(dir: &str) -> Option<String> {
    let mut best: Option<String> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("libopencc.so.") && best.as_ref().is_none_or(|b| name > *b) {
            best = Some(name);
        }
    }
    best
}

/// ggml 靜態建置可能額外產生 CPU 變體庫 (ggml-cpu-haswell 等)。
fn link_extra(lib_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(lib_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_prefix("libggml-cpu-") {
            if let Some(stem) = stem.strip_suffix(".a") {
                println!("cargo:rustc-link-lib=static=ggml-cpu-{stem}");
            }
        }
    }
}
