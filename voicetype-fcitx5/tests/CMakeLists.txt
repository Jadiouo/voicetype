add_executable(test_ipc test_ipc.cpp)
target_link_libraries(test_ipc PRIVATE voicetype-ipc)
add_test(NAME ipc COMMAND test_ipc)
