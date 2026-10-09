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
