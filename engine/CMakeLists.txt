cmake_minimum_required(VERSION 3.16)
project(kuma-preview CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

find_package(PkgConfig REQUIRED)
find_package(Threads REQUIRED)
pkg_check_modules(MLT REQUIRED IMPORTED_TARGET mlt++-7 mlt-framework-7)
pkg_check_modules(SDL2 REQUIRED IMPORTED_TARGET sdl2)
pkg_check_modules(TURBOJPEG REQUIRED IMPORTED_TARGET libturbojpeg)

add_executable(kuma-preview kuma-preview.cpp)
target_link_libraries(kuma-preview PRIVATE PkgConfig::MLT PkgConfig::SDL2 PkgConfig::TURBOJPEG Threads::Threads)
target_compile_options(kuma-preview PRIVATE -Wall -Wextra)
