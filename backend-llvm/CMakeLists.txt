cmake_minimum_required(VERSION 3.20)
project(jihoo-llc LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

find_package(LLVM REQUIRED CONFIG)
message(STATUS "Using LLVM ${LLVM_PACKAGE_VERSION} from ${LLVM_DIR}")

add_executable(jihoo-llc
  src/main.cpp
  src/jir_parser.cpp
  src/codegen.cpp
)

target_include_directories(jihoo-llc SYSTEM PRIVATE ${LLVM_INCLUDE_DIRS})
separate_arguments(LLVM_DEFS NATIVE_COMMAND ${LLVM_DEFINITIONS})
target_compile_definitions(jihoo-llc PRIVATE ${LLVM_DEFS})
if(NOT LLVM_ENABLE_RTTI)
  target_compile_options(jihoo-llc PRIVATE -fno-rtti)
endif()
target_compile_options(jihoo-llc PRIVATE -Wall -Wextra)

if(LLVM_LINK_LLVM_DYLIB)
  target_link_libraries(jihoo-llc PRIVATE LLVM)
else()
  llvm_map_components_to_libnames(LLVM_LIBS
    core support passes target targetparser mc
    ${LLVM_TARGETS_TO_BUILD})
  target_link_libraries(jihoo-llc PRIVATE ${LLVM_LIBS})
endif()

install(TARGETS jihoo-llc RUNTIME DESTINATION bin)

# The backend's own tests: hand-written modules in tests/, independent of the
# frontend (`ctest`, see tests/run-test.sh). Modules that run are linked with
# ld.lld; without it, those tests are skipped.
enable_testing()
find_program(JIHOO_LD NAMES ld.lld)
if(NOT JIHOO_LD)
  set(JIHOO_LD "")
endif()
file(GLOB JIHOO_TESTS CONFIGURE_DEPENDS ${CMAKE_CURRENT_SOURCE_DIR}/tests/*.jir)
foreach(test ${JIHOO_TESTS})
  get_filename_component(name ${test} NAME_WE)
  add_test(NAME ${name}
    COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/tests/run-test.sh
      $<TARGET_FILE:jihoo-llc> "${JIHOO_LD}" ${test} ${CMAKE_CURRENT_BINARY_DIR}/tests)
  set_tests_properties(${name} PROPERTIES SKIP_RETURN_CODE 77)
endforeach()
