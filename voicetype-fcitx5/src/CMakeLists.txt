# NDJSON 編解碼拆成獨立的靜態目標, 讓測試可以連結它而不必連進整個
# addon module (module 目標無法被 link)。
add_library(voicetype-ipc STATIC ipc.cpp)
target_include_directories(voicetype-ipc PUBLIC ${CMAKE_CURRENT_SOURCE_DIR})
target_link_libraries(voicetype-ipc PUBLIC Fcitx5::Utils)
set_target_properties(voicetype-ipc PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(voicetype MODULE voicetype.cpp)
set_target_properties(voicetype PROPERTIES PREFIX "lib")
target_link_libraries(voicetype PRIVATE voicetype-ipc Fcitx5::Core Fcitx5::Utils)

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
