cmake_minimum_required(VERSION 3.16)
project(kuma-preview CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

find_package(PkgConfig REQUIRED)
pkg_check_modules(MLT REQUIRED mlt++-7 mlt-framework-7)
pkg_check_modules(SDL2 REQUIRED sdl2)
pkg_check_modules(TURBOJPEG REQUIRED libturbojpeg)

add_executable(kuma-preview kuma-preview.cpp)
target_include_directories(kuma-preview PRIVATE ${MLT_INCLUDE_DIRS} ${SDL2_INCLUDE_DIRS} ${TURBOJPEG_INCLUDE_DIRS})
target_link_libraries(kuma-preview PRIVATE ${MLT_LIBRARIES} ${SDL2_LIBRARIES} ${TURBOJPEG_LIBRARIES} pthread)
target_compile_options(kuma-preview PRIVATE ${MLT_CFLAGS_OTHER} ${SDL2_CFLAGS_OTHER} -Wall -Wextra)
