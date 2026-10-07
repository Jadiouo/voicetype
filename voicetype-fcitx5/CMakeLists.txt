cmake_minimum_required(VERSION 3.16)
project(voicetype VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE RelWithDebInfo)
endif()

find_package(ECM REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})

find_package(Fcitx5Core REQUIRED)
find_package(Fcitx5Utils REQUIRED)
# Optional at build and runtime; the input-panel fallback needs no extra module.
find_package(Fcitx5ModuleNotifications QUIET)
option(VOICETYPE_X11_CAPS_RESTORE "Preserve Ctrl+Caps Lock state on verified native X11" ON)
option(VOICETYPE_REQUIRE_X11_CAPS_RESTORE "Refuse a release module missing X11 Caps Lock restoration" OFF)
find_package(Fcitx5ModuleXCB QUIET)
find_path(XCB_XKB_INCLUDE_DIR NAMES xcb/xkb.h)
find_path(XCB_EWMH_INCLUDE_DIR NAMES xcb/xcb_ewmh.h HINTS ${XCB_XKB_INCLUDE_DIR})
find_path(XCB_PROTOCOL_INCLUDE_DIR NAMES xcb.h PATH_SUFFIXES xcb)
find_library(XCB_XKB_LIBRARY NAMES xcb-xkb libxcb-xkb.so.1)

# fcitx5 只掃描它自己編譯時決定的 addon 目錄, 不看 CMake 的安裝前綴。
# CMake 預設的 /usr/local 因此會把 .so 裝到一個永遠不會被載入的位置 ——
# 而且 `cmake --install` 會回報成功。這種失敗最貴: 東西「裝好了」,
# 使用者去按熱鍵, 沒反應, 而所有指令都說成功。
#
# Fcitx5Utils 只在安裝前綴恰好等於 fcitx5 自己的前綴時才自動採用它的
# 路徑配置 (見 Fcitx5UtilsConfig.cmake), 所以這裡直接擋下來。
if(NOT FCITX_INSTALL_USE_FCITX_SYS_PATHS AND NOT VOICETYPE_ALLOW_FOREIGN_PREFIX)
    message(FATAL_ERROR
        "安裝前綴 (${CMAKE_INSTALL_PREFIX}) 不是 fcitx5 的前綴, "
        "addon 會裝到 ${FCITX_INSTALL_ADDONDIR} —— fcitx5 不會掃描那裡。\n"
        "  改用:  cmake -B build -DCMAKE_INSTALL_PREFIX=/usr\n"
        "  或:    cmake -B build -DFCITX_INSTALL_USE_FCITX_SYS_PATHS=ON\n"
        "若你確定要裝到別處 (例如自行設定 FCITX_ADDON_DIRS), "
        "加上 -DVOICETYPE_ALLOW_FOREIGN_PREFIX=ON。")
endif()

add_compile_options(-Wall -Wextra)

add_subdirectory(src)
if(VOICETYPE_REQUIRE_X11_CAPS_RESTORE AND NOT VOICETYPE_X11_CAPS_RESTORE_ENABLED)
    message(FATAL_ERROR "Release input module requires native X11 Caps Lock restoration")
endif()

include(CTest)
if(BUILD_TESTING)
    add_subdirectory(tests)
endif()
