# NDJSON 編解碼拆成獨立的靜態目標, 讓測試可以連結它而不必連進整個
# addon module (module 目標無法被 link)。
find_package(Threads REQUIRED)
add_library(voicetype-ipc STATIC ipc.cpp context.cpp selection.cpp)
target_include_directories(voicetype-ipc PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(voicetype-ipc PUBLIC Fcitx5::Utils Threads::Threads)
set_target_properties(voicetype-ipc PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(voicetype-capslock STATIC capslock.cpp)
target_include_directories(voicetype-capslock PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(voicetype-capslock PUBLIC Fcitx5::Core Fcitx5::Utils)
set_target_properties(voicetype-capslock PROPERTIES POSITION_INDEPENDENT_CODE ON)
if(VOICETYPE_X11_CAPS_RESTORE AND TARGET Fcitx5::Module::XCB AND XCB_XKB_INCLUDE_DIR AND XCB_EWMH_INCLUDE_DIR AND XCB_PROTOCOL_INCLUDE_DIR AND XCB_XKB_LIBRARY)
    set(VOICETYPE_X11_CAPS_RESTORE_ENABLED TRUE PARENT_SCOPE)
    target_compile_definitions(voicetype-capslock PRIVATE VOICETYPE_HAVE_X11_CAPS_RESTORE=1)
    target_include_directories(voicetype-capslock PRIVATE ${XCB_XKB_INCLUDE_DIR} ${XCB_EWMH_INCLUDE_DIR} ${XCB_PROTOCOL_INCLUDE_DIR})
    target_link_libraries(voicetype-capslock PRIVATE Fcitx5::Module::XCB ${XCB_XKB_LIBRARY} xcb)
    message(STATUS "VoiceType native X11 Caps Lock preservation: ENABLED")
elseif(VOICETYPE_X11_CAPS_RESTORE)
    message(WARNING "VoiceType native X11 Caps Lock preservation: DISABLED (requires Fcitx5ModuleXCB, libxcb-ewmh-dev and libxcb-xkb-dev)")
else()
    message(STATUS "VoiceType native X11 Caps Lock preservation: DISABLED by build option")
endif()

add_library(voicetype MODULE voicetype.cpp)
set_target_properties(voicetype PROPERTIES PREFIX "lib")
target_link_libraries(voicetype PRIVATE voicetype-ipc voicetype-capslock Fcitx5::Core Fcitx5::Utils)
if(TARGET Fcitx5::Module::Notifications)
    target_link_libraries(voicetype PRIVATE Fcitx5::Module::Notifications)
    target_compile_definitions(voicetype PRIVATE VOICETYPE_HAVE_NOTIFICATIONS=1)
endif()

install(TARGETS voicetype DESTINATION "${FCITX_INSTALL_LIBDIR}/fcitx5")

# addon 的註冊檔。沒有它 fcitx5 不會載入 module —— .so 裝好了也一樣。
#
# 刻意**不**用 `fcitx5_translate_desktop_file()`: 那個宏跑 msgfmt 把
# po/ 的翻譯併進 .conf, 但本專案沒有 po/ 目錄, 翻譯直接寫在範本裡。
# 對我們它是 no-op, 卻會在 GETTEXT_MSGFMT_EXECUTABLE 未定義時把命令
# 展開成空字串 —— make 印出「Generating voicetype.conf」、回傳成功、
# 不產生任何檔案。安裝階段才會炸, 而錯誤訊息指向檔案不存在, 不會指向
# 真正的原因。
#
# 之後真的要加 po/ 翻譯時再換回那個宏, 並記得 `find_package(Gettext)`。
configure_file(voicetype.conf.in voicetype.conf @ONLY)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/voicetype.conf"
        DESTINATION "${FCITX_INSTALL_PKGDATADIR}/addon")
