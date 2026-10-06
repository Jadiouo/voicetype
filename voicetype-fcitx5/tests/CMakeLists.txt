add_executable(test_ipc test_ipc.cpp)
target_link_libraries(test_ipc PRIVATE voicetype-ipc)
add_test(NAME ipc COMMAND test_ipc)

add_executable(test_context test_context.cpp)
target_link_libraries(test_context PRIVATE voicetype-ipc)
add_test(NAME context COMMAND test_context)

add_executable(test_lifecycle test_lifecycle.cpp ../src/voicetype.cpp)
target_link_libraries(test_lifecycle PRIVATE voicetype-ipc voicetype-capslock Fcitx5::Core)
if(TARGET Fcitx5::Module::Notifications)
    target_link_libraries(test_lifecycle PRIVATE Fcitx5::Module::Notifications)
    target_compile_definitions(test_lifecycle PRIVATE VOICETYPE_HAVE_NOTIFICATIONS=1)
endif()
add_test(NAME lifecycle COMMAND test_lifecycle)

add_executable(test_delivery test_delivery.cpp ../src/voicetype.cpp)
target_link_libraries(test_delivery PRIVATE voicetype-ipc voicetype-capslock Fcitx5::Core)
if(TARGET Fcitx5::Module::Notifications)
    target_link_libraries(test_delivery PRIVATE Fcitx5::Module::Notifications)
    target_compile_definitions(test_delivery PRIVATE VOICETYPE_HAVE_NOTIFICATIONS=1)
endif()
foreach(scenario same_context duplicate old_session unfocused_start focus_out focus_return focus_recording
                 reset reset_recording sensitive_return sensitive_recording password_return
                 cancel_processing cancel_recording cancel_destroy disconnect_processing
                 disconnect_recording disconnect_destroy error_result error_destroy_result
                 destroy_result destroy_error_result empty_result preedit_reset preedit_focus_return
                 preedit_sensitive_return preedit_new_session unrelated_reset unrelated_focus_out
                 groupless_destroy_result groupless_destroy_error_result groupless_cancel_destroy)
    add_test(NAME delivery_${scenario} COMMAND test_delivery ${scenario})
    set_tests_properties(delivery_${scenario} PROPERTIES TIMEOUT 10)
endforeach()

add_executable(test_feedback test_feedback.cpp ../src/voicetype.cpp)
target_link_libraries(test_feedback PRIVATE voicetype-ipc voicetype-capslock Fcitx5::Core)
if(TARGET Fcitx5::Module::Notifications)
    target_link_libraries(test_feedback PRIVATE Fcitx5::Module::Notifications)
    target_compile_definitions(test_feedback PRIVATE VOICETYPE_HAVE_NOTIFICATIONS=1)
endif()
add_test(NAME feedback COMMAND test_feedback)

add_executable(test_learnkey_config test_learnkey_config.cpp ../src/voicetype.cpp)
target_link_libraries(test_learnkey_config PRIVATE voicetype-ipc voicetype-capslock Fcitx5::Core)
if(TARGET Fcitx5::Module::Notifications)
    target_link_libraries(test_learnkey_config PRIVATE Fcitx5::Module::Notifications)
    target_compile_definitions(test_learnkey_config PRIVATE VOICETYPE_HAVE_NOTIFICATIONS=1)
endif()
add_test(NAME learnkey_config COMMAND test_learnkey_config)

add_executable(test_autolearn_submit test_autolearn_submit.cpp ../src/voicetype.cpp)
target_link_libraries(test_autolearn_submit PRIVATE voicetype-ipc voicetype-capslock Fcitx5::Core)
if(TARGET Fcitx5::Module::Notifications)
    target_link_libraries(test_autolearn_submit PRIVATE Fcitx5::Module::Notifications)
    target_compile_definitions(test_autolearn_submit PRIVATE VOICETYPE_HAVE_NOTIFICATIONS=1)
endif()
add_test(NAME autolearn_submit COMMAND test_autolearn_submit)
foreach(scenario ctrl_enter mouse fast_enter fast_ctrl_enter fast_mouse keep anchored
                 app_only app_rewrite keypress_app_rewrite paste shift_paste manual_paste undo focus cancel delete stale composition newline split_replace split_arbitrary backspace_ascii delete_ascii caps_ascii ctrl_backspace ctrl_delete
                 virtual_ascii preedit_ascii wrong_caret multikey_ascii caps_mismatch)
    add_test(NAME autolearn_${scenario} COMMAND test_autolearn_submit ${scenario})
endforeach()
foreach(scenario asr_internal asr_model_load_failed asr_too_long asr_no_audio_device asr_ui stale_error control_error)
    add_test(NAME ${scenario} COMMAND test_autolearn_submit ${scenario})
endforeach()
find_package(Python3 COMPONENTS Interpreter REQUIRED)
add_test(NAME selection_helper_privacy COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/test_selection_helper.py)

foreach(scenario selection_success selection_error selection_timeout selection_focus selection_wrong_program)
    add_test(NAME ${scenario} COMMAND test_autolearn_submit ${scenario})
endforeach()

add_executable(test_selection_reader test_selection_reader.cpp)
target_link_libraries(test_selection_reader PRIVATE voicetype-ipc)
add_test(NAME selection_reader COMMAND test_selection_reader)
set_tests_properties(selection_reader PROPERTIES TIMEOUT 10)

add_executable(test_capslock test_capslock.cpp)
target_link_libraries(test_capslock PRIVATE voicetype-capslock)
add_test(NAME capslock_gesture COMMAND test_capslock)

# Optional native regression test: never targets the user's inherited display.
find_package(X11 QUIET)
find_library(XTEST_LIBRARY NAMES Xtst libXtst.so.6)
find_program(XVFB_EXECUTABLE Xvfb)
if(VOICETYPE_X11_CAPS_RESTORE_ENABLED AND X11_FOUND AND XTEST_LIBRARY AND XVFB_EXECUTABLE)
    add_executable(test_capslock_x11 test_capslock_x11.cpp ../src/voicetype.cpp)
    target_link_libraries(test_capslock_x11 PRIVATE voicetype-ipc voicetype-capslock X11::X11 ${XTEST_LIBRARY})
    add_test(NAME capslock_native_x11 COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/run_xvfb.py ${XVFB_EXECUTABLE} $<TARGET_FILE:test_capslock_x11>)
    set_tests_properties(capslock_native_x11 PROPERTIES TIMEOUT 20)
    add_test(NAME capslock_native_x11_remapped COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/run_xvfb.py ${XVFB_EXECUTABLE} $<TARGET_FILE:test_capslock_x11> remapped)
    set_tests_properties(capslock_native_x11_remapped PROPERTIES TIMEOUT 20)
else()
    message(STATUS "Native Caps Lock integration test unavailable (needs enabled XCB support, X11/XTest and Xvfb)")
endif()
