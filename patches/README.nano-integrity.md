# sherpa-onnx Nano 完整性修正

此 patch 針對 **sherpa-onnx v1.13.8** 的 Nano 解碼器，保留模型、提示詞、ITN、512 上限與 greedy 參數。原實作會截掉過長輸入的 audio placeholders，或在尚未產生 EOS 時回傳部分文字；修正後這些結果明確失敗，不交付給輸入框。這是 optional Nano backend 的完整性條件，不代表它已部署或辨識率提升。

來源為 [k2-fsa/sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx/tree/11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf)，遵循上游 **Apache-2.0** 授權。Patch 保留原檔著作權，修改檔案只有 `offline-recognizer-funasr-nano-impl.cc` 與 `.h`。使用／分發重建程式庫時，應一併保留上游 LICENSE 與各相依套件授權。

## 固定來源

Source commit：`11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf`。

使用 [固定 commit archive](https://codeload.github.com/k2-fsa/sherpa-onnx/tar.gz/11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf)，SHA256：`0a8db6c55dd318f4a688faba85f7760b99a6c92e8ef8864479d418531bee1ac2`。在乾淨來源目錄套用：

```sh
patch --directory "$nano_source" --strip=1 < patches/sherpa-onnx-1.13.8-nano-integrity.patch
```

Archive 含與此建置無關的範例絕對 symlink；解包只取一般檔案／目錄或所需的 `CMakeLists.txt`、`cmake/`、`sherpa-onnx/` 與 LICENSE，不要允許 archive link 寫到目的地之外。

以下是上游 CMake **實際解析**的依賴版本與 archive SHA256。頂層 `cmake/` 會覆蓋 transitive Eigen／OpenFst 的舊版本，不能只看 kaldi-decoder 自己的清單。

| 官方來源／版本 | SHA256 |
|---|---|
| [kaldi-native-fbank v1.22.3](https://github.com/csukuangfj/kaldi-native-fbank/tree/v1.22.3) | `9176cc66fc7ce1edf85cf355b06e320c57db6297df74277f575183468893cf61` |
| [kaldi-decoder v0.3.0](https://github.com/k2-fsa/kaldi-decoder/tree/v0.3.0) | `b9f34cfb4fd3b1344100eead79ef4d37aa15962274b9e3056de345021f76a1b0` |
| [kaldifst v1.8.0](https://github.com/k2-fsa/kaldifst/tree/v1.8.0) | `3f247b7e5a2409071202f5e2bc6200060f66728c0a3443c03923ad2723e040b3` |
| [OpenFst v1.8.5-2026-07-09](https://github.com/csukuangfj/openfst/tree/v1.8.5-2026-07-09) | `2ff712a32952fcb01d351121a6bc8ccf4fdc6b2aa06ce8df2b3095dedd518c0e` |
| [Eigen 5.0.1](https://gitlab.com/libeigen/eigen/-/tree/5.0.1) | `e9c326dc8c05cd1e044c71f30f1b2e34a6161a3b6ecf445d56b53ff1669e3dec` |
| [simple-sentencepiece v0.7](https://github.com/pkufool/simple-sentencepiece/tree/v0.7) | `1748a822060a35baa9f6609f84efc8eb54dc0e74b9ece3d82367b7119fdc75af` |
| [JSON v3.12.0](https://github.com/nlohmann/json/tree/v3.12.0) | `4b92eb0c06d10683f7447ce9406cb97cd4b453be18d7279320f7b2f025c10187` |
| [KissFFT febd4cae…](https://github.com/mborgerding/kissfft/tree/febd4caeed32e33ad8b2e0bb5ea77542c40f18ec)（zip） | `497103e664168ebe39580b757adbe616f6cf85a16572af581ca7bc42d0ab13fd` |

除 KissFFT 使用 commit zip，其餘使用對應 tag 的 GitHub codeload tar.gz，Eigen 使用 GitLab tag archive。先核 archive hash，再解到隔離目錄，透過 `FETCHCONTENT_SOURCE_DIR_<NAME>` 指定本機來源（名稱為 `KALDI_NATIVE_FBANK`、`KALDI_DECODER`、`KALDIFST`、`OPENFST`、`EIGEN`、`SIMPLE-SENTENCEPIECE`、`JSON`、`KISSFFT`）。設 `FETCHCONTENT_FULLY_DISCONNECTED=ON`，避免 configure 偷抓不同資產。

ORT 直接重用官方 CPU **1.28.2** shared library，SHA256 `4b3607aebd1784b26b6f9b20e4bd974c7ab8287043e4d095cb7d2cb40b5e566e`。可從 sherpa 官方 `sherpa-onnx-v1.13.8-linux-x64-shared-no-tts-lib.tar.bz2` 取得；archive SHA256 `bf2d998c8b07012cd5098f3b92673bc1333fd9b927767d7cb664be8190d8bc0b`。不重建 ORT，不取得 GPU 或模型套件。編譯標頭使用 [Microsoft ORT v1.28.2 session headers](https://github.com/microsoft/onnxruntime/tree/v1.28.2/include/onnxruntime/core/session)，包含 `onnxruntime_c_api.h`、`onnxruntime_cxx_api.h`、`onnxruntime_cxx_inline.h`、`onnxruntime_float16.h`、`onnxruntime_error_code.h`、`onnxruntime_ep_c_api.h`，不混用系統其他版本。

## 建置邊界

用 CMake/Ninja、C++17，在隔離 build 目錄只建 `sherpa-onnx-c-api`。`SHERPA_ONNXRUNTIME_INCLUDE_DIR` 與 `SHERPA_ONNXRUNTIME_LIB_DIR` 環境變數必須指向上述配對。設定：

```text
CMAKE_BUILD_TYPE=Release
BUILD_SHARED_LIBS=ON
CMAKE_SHARED_LINKER_FLAGS=-Wl,--disable-new-dtags
FETCHCONTENT_FULLY_DISCONNECTED=ON
SHERPA_ONNX_ENABLE_C_API=ON
SHERPA_ONNX_USE_PRE_INSTALLED_ONNXRUNTIME_IF_AVAILABLE=ON
SHERPA_ONNX_BUILD_C_API_EXAMPLES=OFF
```

將 `SHERPA_ONNX_ENABLE_` 後接 `GPU`、`DIRECTML`、`TTS`、`SPEAKER_DIARIZATION`、`PYTHON`、`JNI`、`BINARY`、`WEBSOCKET`、`PORTAUDIO`、`TESTS`、`RKNN`、`AXERA`、`AXCL`、`ASCEND_NPU`、`QNN`、`SPACEMIT` 的選項全部設 `OFF`。Core 仍會編譯其他 ASR 的 C++；這不代表常駐其他模型。建置指令為 `cmake --build "$nano_build" --target sherpa-onnx-c-api -j4`。

依建置環境設定合理的CPU、記憶體和時間上限。用kernel cgroup觀察實際限制與峰值，不能用回收後摘要取代量測。

封裝只取 C API `.so`、原 ORT `.so`、未修改的 `c-api.h` 與授權檔。成品 C API 的 DT_RPATH 應僅為 `$ORIGIN`，去掉 build／其他套件目錄；核對完整 `readelf`／`ldd` 依賴樹及實際 maps。這次只有 ORT 是非系統動態依賴，其餘 core 依賴靜態連結。重新計算成品 hash，再交由應用端固定；不同編譯環境的產物不能冒用本機測得的 hash。

特別注意：該 source archive 的 `version.cc` 本來硬編 `GetGitSha1() = "8c8e275d"`，官方 prebuilt 則回 `11afbd00`。本 patch 不改版本檔。**Reported git string 不是來源證明**；應同時核 source commit/archive hash、patch hash、成品 hash及 runtime 實際字串。C API header SHA256 保持 `2a1b95084be8fd1deb3228fcad2fd3f7f0258b64582f7402281ec174c7b7f4ce`。

## 完整性協定與驗證

每個新 stream 設 `voicetype.nano.integrity_version=1`、`voicetype.nano.stop_reason=unknown`；每次 decode 在任何 feature/model 操作之前重設整個 batch 的 status 與 result。Shim 不得自行寫成功 marker。Decode 後在 stream 存活期間讀取既有 C API GetOption：

- `eos`：實際取到 EOS 或 im_end；允許結果，包括正常空結果。
- `no_speech`：零 source frames 或既有數位靜音捷徑；只允許空文字。
- `audio_truncated`、`context_limit`、`max_new_tokens`、`input_too_short`、`invalid_model_output`、`unknown`：失敗，不送出部分文字。
- 缺 marker、未知 version/status、`no_speech` 帶非空文字：失敗。原版 library 不可冒充通過。

另外防止 LFR 在不足完整 window 時越界；非空但太短的 features 以 `input_too_short` 失敗，不能宣稱無語音。正常 LFR 路徑不變。

驗證必須涵蓋實際 generation loop 的 EOS／im_end、context 最後一步 EOS 與非 EOS、token 上限最後一步、輸入截斷、shape/alignment、stream 重用與 batch 中首個例外後的舊 status。受控 backend 可測邊界，但不可稱為真模型準確度測試。真正 native library 還需驗 stock marker 拒絕、固定音訊 parity、VAD、資源與服務錯誤提示；本 patch 不能自動繼承舊 Python benchmark 分數。
