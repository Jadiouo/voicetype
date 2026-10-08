cmake_minimum_required(VERSION 3.24)
project(voicetype_tsf_probe LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC)
  message(FATAL_ERROR "This experiment requires native Windows MSVC and the Windows SDK")
endif()

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
add_compile_options(/W4 /WX /utf-8 /permissive-)

add_library(voicetype_tsf_probe SHARED speech_probe.cpp speech_probe.def)
target_compile_definitions(voicetype_tsf_probe PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN)
target_link_libraries(voicetype_tsf_probe PRIVATE ole32 uuid user32)

add_executable(voicetype_tsf_native_fixture native_fixture.cpp text_store_fixture.cpp)
target_compile_definitions(voicetype_tsf_native_fixture PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN)
target_link_libraries(voicetype_tsf_native_fixture PRIVATE ole32 uuid user32)

add_executable(voicetype_tsf_registrar registrar.cpp)
target_compile_definitions(voicetype_tsf_registrar PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN)
target_link_libraries(voicetype_tsf_registrar PRIVATE ole32 uuid advapi32)

add_executable(voicetype_tsf_registrar_policy_probe registrar_policy_probe.cpp)

add_executable(voicetype_tsf_b_target b_target.cpp text_store_fixture.cpp)
target_compile_definitions(voicetype_tsf_b_target PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN)
target_link_libraries(voicetype_tsf_b_target PRIVATE ole32 uuid user32)

add_executable(voicetype_tsf_broker_probe broker_probe.cpp)
target_compile_definitions(voicetype_tsf_broker_probe PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN)

enable_testing()
add_test(NAME tsf_native_acp_once
  COMMAND voicetype_tsf_native_fixture $<TARGET_FILE:voicetype_tsf_probe>)
add_test(NAME tsf_preserved_key_fixture
  COMMAND voicetype_tsf_native_fixture $<TARGET_FILE:voicetype_tsf_probe> --preserved)
add_test(NAME tsf_broker_protocol
  COMMAND voicetype_tsf_broker_probe)
add_test(NAME tsf_registrar_status_readonly
  COMMAND voicetype_tsf_registrar status)
add_test(NAME tsf_registrar_rollback_policy
  COMMAND voicetype_tsf_registrar_policy_probe)
