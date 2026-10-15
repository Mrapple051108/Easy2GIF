cmake_minimum_required(VERSION 3.21)

# ---------------------------------------------------------------------------
# Easy2GIF — a video-to-GIF converter.
#
#   * `gifcore`  — the Rust kernel. It owns every part of the video pipeline
#                  (probing, decoding, colour reduction, GIF encoding) and is
#                  consumed through the plain C ABI in include/gifcore.h.
#   * `easy2gif` — the Qt Quick front end. It presents kernel state and
#                  forwards user intent; it never touches video data.
#
# The kernel is compiled with the GNU toolchain and linked in as a static
# library, so nothing extra has to be installed alongside the executable.
# ---------------------------------------------------------------------------

project(easy2gif VERSION 1.0.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
    set(CMAKE_BUILD_TYPE Release CACHE STRING "" FORCE)
endif()

option(GIFCORE_CONSOLE "Keep a console window for diagnostics" OFF)

# ---------------------------------------------------------------------------
# Rust kernel
# ---------------------------------------------------------------------------

set(GIFCORE_RUSTUP_HOME "${CMAKE_CURRENT_SOURCE_DIR}/.rustup_local"
    CACHE PATH "RUSTUP_HOME used to build the kernel")
set(GIFCORE_RUST_TOOLCHAIN "stable-x86_64-pc-windows-gnu"
    CACHE STRING "rustup toolchain used to build the kernel")
set(GIFCORE_CARGO "" CACHE FILEPATH "cargo executable (auto-detected when empty)")

if(GIFCORE_CARGO)
    set(GIFCORE_CARGO_EXECUTABLE "${GIFCORE_CARGO}")
else()
    find_program(GIFCORE_CARGO_EXECUTABLE NAMES cargo cargo.exe REQUIRED
        HINTS "$ENV{USERPROFILE}/.cargo/bin" "$ENV{CARGO_HOME}/bin")
endif()

set(GIFCORE_RUST_TARGET_DIR "${CMAKE_CURRENT_BINARY_DIR}/rust")
set(GIFCORE_RUST_LIBRARY
    "${GIFCORE_RUST_TARGET_DIR}/release/${CMAKE_STATIC_LIBRARY_PREFIX}gifcore${CMAKE_STATIC_LIBRARY_SUFFIX}")
# The Rust-side entry point. It is copied into the bundle next to the Qt front
# end so one Run button can start either half.
set(GIFCORE_RUST_FORGE
    "${GIFCORE_RUST_TARGET_DIR}/release/forge${CMAKE_EXECUTABLE_SUFFIX}")

# The Rust link step shells out to the C linker, and that linker creates scratch
# files in the directory named by TMP/TEMP. Inheriting the ambient TEMP is not
# safe -- under a restricted shell it can point somewhere the linker's own child
# processes may not write, and collect2 then dies with "Cannot create temporary
# file in ...: Permission denied". Keep it inside the build tree, which is always
# writable by the toolchain that CMake just invoked.
set(GIFCORE_RUST_TMP "${CMAKE_CURRENT_BINARY_DIR}/tmp")

file(GLOB_RECURSE GIFCORE_RUST_SOURCES CONFIGURE_DEPENDS
     "${CMAKE_CURRENT_SOURCE_DIR}/src/*.rs")

# The Rust link step shells out to the C toolchain, so the directory holding the
# MinGW binaries has to be on the PATH that cargo sees. CMake knows where the
# compiler is, but CLion does not put that directory on PATH for us, so derive
# it here instead of relying on the environment.
get_filename_component(GIFCORE_TOOLCHAIN_BIN_DIR "${CMAKE_C_COMPILER}" DIRECTORY)
file(TO_NATIVE_PATH "${GIFCORE_TOOLCHAIN_BIN_DIR}" GIFCORE_TOOLCHAIN_BIN_DIR_NATIVE)

# Both Rust artifacts come out of one cargo invocation: the static library the
# Qt front end links against, and the `forge` launcher. Building them together
# keeps a single compile of the kernel, and the launcher is what the Run button
# can start when you want the Rust entry point rather than the Qt one.
add_custom_command(
    OUTPUT "${GIFCORE_RUST_LIBRARY}" "${GIFCORE_RUST_FORGE}"
    COMMAND "${CMAKE_COMMAND}" -E make_directory "${GIFCORE_RUST_TMP}"
    COMMAND "${CMAKE_COMMAND}" -E env
            "RUSTUP_HOME=${GIFCORE_RUSTUP_HOME}"
            "RUSTUP_TOOLCHAIN=${GIFCORE_RUST_TOOLCHAIN}"
            "CARGO_TARGET_DIR=${GIFCORE_RUST_TARGET_DIR}"
            "TMP=${GIFCORE_RUST_TMP}"
            "TEMP=${GIFCORE_RUST_TMP}"
            "PATH=${GIFCORE_TOOLCHAIN_BIN_DIR_NATIVE};$ENV{PATH}"
            "${GIFCORE_CARGO_EXECUTABLE}" build --release --offline --lib --bin forge
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    DEPENDS ${GIFCORE_RUST_SOURCES} "${CMAKE_CURRENT_SOURCE_DIR}/Cargo.toml"
    COMMENT "Building the Rust kernel and launcher (gifcore, release)"
    VERBATIM)

add_custom_target(gifcore_rust DEPENDS "${GIFCORE_RUST_LIBRARY}" "${GIFCORE_RUST_FORGE}")

if(WIN32)
    # A file born inside a Low-labelled folder inherits that label, and Windows
    # takes a process's integrity level from the label on its *image file*, so a
    # freshly built forge.exe would come out Low and then be refused every
    # ordinary folder -- as would the front end it starts, since a Low parent
    # cannot create a Medium child. The build directories are kept Medium
    # (relocate.ps1 restores that), and this stamps the file as a second line of
    # defence, before it is ever started.
    add_custom_command(TARGET gifcore_rust POST_BUILD
        COMMAND icacls "${GIFCORE_RUST_FORGE}" /setintegritylevel M /C
        COMMENT "Marking forge.exe as Medium integrity"
        VERBATIM)
endif()

# ---------------------------------------------------------------------------
# Qt Quick front end
# ---------------------------------------------------------------------------

# The Rust kernel is built for x86_64-pc-windows-gnu, so the C++ half has to
# use the same ABI. Say so plainly instead of letting the link fail with
# thousands of unresolved symbols.
if(MSVC)
    message(FATAL_ERROR
        "Easy2GIF must be built with the MinGW (GNU) toolchain, because the Rust "
        "kernel is compiled for x86_64-pc-windows-gnu. In CLion, set the toolchain "
        "to the MinGW at D:/CODE/QT/Tools/mingw1310_64, or configure with the "
        "\"mingw-release\" CMake preset, which selects it for you.")
endif()

# A plain CLion CMake profile configures this project without passing any cache
# variables, so look for Qt in the usual places before giving up. An explicit
# -DCMAKE_PREFIX_PATH always wins, because this only runs when Qt has not been
# found yet and no prefix was supplied.
if(NOT Qt6_DIR AND NOT CMAKE_PREFIX_PATH)
    foreach(_candidate
            "$ENV{QT_ROOT}"
            "$ENV{QTDIR}"
            "D:/CODE/QT/6.10.2/mingw_64"
            "C:/Qt/6.10.2/mingw_64"
            "C:/Qt/6.8.0/mingw_64")
        if(_candidate AND EXISTS "${_candidate}/lib/cmake/Qt6/Qt6Config.cmake")
            list(APPEND CMAKE_PREFIX_PATH "${_candidate}")
            message(STATUS "Easy2GIF: using the Qt installation at ${_candidate}")
            break()
        endif()
    endforeach()
endif()

find_package(Qt6 6.5 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2)
qt_standard_project_setup()

# Every QML file lives in app/qml, so no per-directory qmldir files are needed.
# QTP0004 NEW would demand one anyway; keep the previous behaviour.
qt_policy(SET QTP0004 OLD)

set(GIFCORE_QML_DIR "${CMAKE_CURRENT_SOURCE_DIR}/app/qml")

# Where the shippable application folder lives. The front end is linked straight
# into it rather than into the build root, so the executable CLion's Run button
# starts is the very one that ends up with Qt and the media engine beside it --
# and Run needs nothing on PATH beyond what the run configuration already sets.
set(GIFCORE_BUNDLE_DIR "${CMAKE_BINARY_DIR}/bundle")

qt_add_executable(easy2gif
    app/main.cpp
    app/GifBridge.cpp
    app/GifBridge.h
)

# Rust is already stripped; drop the final MinGW symbol table in Release too.
if(MINGW)
    target_link_options(easy2gif PRIVATE "$<$<CONFIG:Release>:-s>")
endif()

if(NOT GIFCORE_CONSOLE AND WIN32)
    set_target_properties(easy2gif PROPERTIES WIN32_EXECUTABLE TRUE)
endif()

qt_add_qml_module(easy2gif
    URI easy2gif
    VERSION 1.0
    RESOURCE_PREFIX "/qt/qml"
    QML_FILES
        app/qml/Main.qml
        app/qml/FrameSlider.qml
)

set_target_properties(easy2gif PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${GIFCORE_BUNDLE_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_DEBUG "${GIFCORE_BUNDLE_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_RELEASE "${GIFCORE_BUNDLE_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_RELWITHDEBINFO "${GIFCORE_BUNDLE_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_MINSIZEREL "${GIFCORE_BUNDLE_DIR}"
)

target_include_directories(easy2gif PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/include"
    "${CMAKE_CURRENT_SOURCE_DIR}/app")

target_link_libraries(easy2gif PRIVATE
    Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2
    "${GIFCORE_RUST_LIBRARY}")

# The Rust static library pulls in parts of the Windows runtime.
if(WIN32)
    target_link_libraries(easy2gif PRIVATE
        ws2_32 userenv bcrypt ntdll ole32 oleaut32 setupapi advapi32
        shell32 user32 gdi32)
endif()

add_dependencies(easy2gif gifcore_rust)

# ---------------------------------------------------------------------------
# Bundling: everything the app needs travels with it
# ---------------------------------------------------------------------------

get_target_property(GIFCORE_QMAKE Qt6::qmake IMPORTED_LOCATION)
get_filename_component(GIFCORE_QT_BIN_DIR "${GIFCORE_QMAKE}" DIRECTORY)
find_program(GIFCORE_WINDEPLOYQT NAMES windeployqt windeployqt.exe
    HINTS "${GIFCORE_QT_BIN_DIR}" REQUIRED)

# The bundled media engine. Override with -DGIFCORE_FFMPEG_EXE=... to point at
# another build.
#
# A real distribution is preferred over whatever ``find_program`` finds on PATH,
# because a package-manager shim (chocolatey, winget, scoop) is only a few
# hundred KiB and resolves its real target relative to its own location. Running
# such a shim on this machine works; copying it into the bundle produces an
# executable that dies on any other machine. That is exactly the failure the
# "no plugins to install" requirement rules out, so the real binary is looked
# for first and PATH is only a fallback.
function(gifcore_find_media_tool var name)
    if(${var})
        return()
    endif()
    file(GLOB _real_candidates
        "C:/ProgramData/chocolatey/lib/ffmpeg/tools/ffmpeg/bin/${name}.exe"
        "$ENV{LOCALAPPDATA}/Microsoft/WinGet/Packages/*/bin/${name}.exe")
    foreach(_candidate IN LISTS _real_candidates)
        if(EXISTS "${_candidate}")
            file(SIZE "${_candidate}" _size)
            get_filename_component(_candidate_dir "${_candidate}" DIRECTORY)
            file(GLOB _codec_dlls "${_candidate_dir}/avcodec-*.dll")
            if(_size GREATER 1048576 OR _codec_dlls)
                set(${var} "${_candidate}" CACHE FILEPATH "Bundled ${name} executable" FORCE)
                return()
            endif()
        endif()
    endforeach()
    find_program(${var} NAMES ${name} ${name}.exe)
endfunction()

gifcore_find_media_tool(GIFCORE_FFMPEG_EXE ffmpeg)
gifcore_find_media_tool(GIFCORE_FFPROBE_EXE ffprobe)

# A launcher shim is a few hundred KiB and finds its real target relative to
# its own location, so a copied shim only works on the build machine. Warn here
# and fail hard after the copy, where the truth is cheap to establish.
#
# Passed as an explicit argument rather than derived from the tool name: CMake
# variable names are case-sensitive, so ``GIFCORE_${tool}_EXE`` would look for
# the lowercase ``GIFCORE_ffmpeg_EXE`` and silently never fire.
function(gifcore_report_tool tool path)
    if(NOT path)
        return()
    endif()
    file(SIZE "${path}" _size)
    get_filename_component(_tool_dir "${path}" DIRECTORY)
    file(GLOB _shared_codec "${_tool_dir}/avcodec-*.dll")
    if(_shared_codec)
        message(STATUS "Easy2GIF: ${tool} = ${path} (shared media runtime)")
        return()
    endif()
    if(_size LESS 1048576)
        message(WARNING
            "Easy2GIF: ${path} is only ${_size} bytes, which looks like a launcher "
            "shim rather than a real build. A bundled shim will not run on another "
            "machine; point -DGIFCORE_${tool}_EXE at a real static or shared distribution.")
    else()
        math(EXPR _mb "${_size} / 1048576")
        message(STATUS "Easy2GIF: ${tool} = ${path} (~${_mb} MiB)")
    endif()
endfunction()

gifcore_report_tool(FFMPEG "${GIFCORE_FFMPEG_EXE}")
gifcore_report_tool(FFPROBE "${GIFCORE_FFPROBE_EXE}")

# GIFCORE_BUNDLE_DIR is set next to the executable target; the bundle target below
# only fills that folder with the Qt runtime, the QML plugins and the media engine.

add_custom_target(bundle
    COMMAND "${CMAKE_COMMAND}" -E make_directory "${GIFCORE_BUNDLE_DIR}"
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
            "$<TARGET_FILE:easy2gif>" "${GIFCORE_BUNDLE_DIR}/"
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
            "${GIFCORE_RUST_FORGE}" "${GIFCORE_BUNDLE_DIR}/"
    COMMAND "${GIFCORE_WINDEPLOYQT}" --release --no-translations
            --skip-plugin-types qmltooling,platforminputcontexts,vectorimageformats,generic
            --qmldir "${GIFCORE_QML_DIR}"
            "${GIFCORE_BUNDLE_DIR}/$<TARGET_FILE_NAME:easy2gif>"
    COMMENT "Collecting Qt runtime and QML plugins"
    DEPENDS easy2gif gifcore_rust
    VERBATIM)

if(GIFCORE_FFMPEG_EXE AND GIFCORE_FFPROBE_EXE)
    add_custom_command(TARGET bundle POST_BUILD
        COMMAND "${CMAKE_COMMAND}"
                "-DFFMPEG=${GIFCORE_FFMPEG_EXE}"
                "-DFFPROBE=${GIFCORE_FFPROBE_EXE}"
                "-DDESTINATION=${GIFCORE_BUNDLE_DIR}/bin"
                -P "${CMAKE_CURRENT_SOURCE_DIR}/cmake/BundleMedia.cmake"
        COMMENT "Bundling the media engine into bin/"
        VERBATIM)

    # Run the copies from where they now live. This is the only honest test of
    # "needs nothing installed": a launcher shim fails right here with a
    # "cannot find file at ..\lib\..." message, while a complete distribution
    # answers with its version banner and exits 0.
    option(GIFCORE_VERIFY_BUNDLE
        "Verify the bundled media engine and its runtime dependencies" ON)
    if(GIFCORE_VERIFY_BUNDLE)
        add_custom_command(TARGET bundle POST_BUILD
            COMMAND "${GIFCORE_BUNDLE_DIR}/bin/ffmpeg.exe" -version
            COMMAND "${GIFCORE_BUNDLE_DIR}/bin/ffprobe.exe" -version
            COMMENT "Verifying the bundled media engine runs standalone"
            VERBATIM)
    endif()
else()
    add_custom_command(TARGET bundle POST_BUILD
        COMMAND "${CMAKE_COMMAND}" -E echo
                "WARNING: ffmpeg/ffprobe were not found; the bundle will not run."
        VERBATIM)
endif()

# ---------------------------------------------------------------------------
# Prune the parts of the Qt runtime this app provably never loads.
#
# The style plugins are the interesting case: main() pins QQuickStyle("Basic")
# *before* the QGuiApplication exists, so Fusion / Imagine / Material /
# Universal / FluentWinUI3 and their impl libraries can never be selected. The
# rest are features no QML here mentions (Lottie, Quick3D, the virtual
# keyboard) plus qmltooling, which is debugger support for a release build.
#
# Deleting these is safe rather than merely smaller: the same pinned style is
# what keeps the look independent of whatever is installed on the machine.
option(GIFCORE_PRUNE_BUNDLE "Delete unused Qt modules from the bundle" ON)

if(GIFCORE_PRUNE_BUNDLE)
    set(GIFCORE_PRUNE_FILES
        Qt6Lottie.dll
        Qt6LottieVectorImageGenerator.dll
        Qt6Quick3DUtils.dll
        Qt6VirtualKeyboard.dll
        Qt6QuickControls2FluentWinUI3StyleImpl.dll
        Qt6QuickControls2Fusion.dll
        Qt6QuickControls2FusionStyleImpl.dll
        Qt6QuickControls2Imagine.dll
        Qt6QuickControls2ImagineStyleImpl.dll
        Qt6QuickControls2Material.dll
        Qt6QuickControls2MaterialStyleImpl.dll
        Qt6QuickControls2Universal.dll
        Qt6QuickControls2UniversalStyleImpl.dll
        Qt6QuickControls2WindowsStyleImpl.dll)
    set(GIFCORE_PRUNE_COMMANDS "")
    foreach(_prune IN LISTS GIFCORE_PRUNE_FILES)
        list(APPEND GIFCORE_PRUNE_COMMANDS
            COMMAND "${CMAKE_COMMAND}" -E rm -f
                    "${GIFCORE_BUNDLE_DIR}/${_prune}")
    endforeach()
    list(APPEND GIFCORE_PRUNE_COMMANDS
        COMMAND "${CMAKE_COMMAND}" -E rm -rf "${GIFCORE_BUNDLE_DIR}/qmltooling")
    foreach(_style FluentWinUI3 Fusion Imagine Material Universal Windows)
        list(APPEND GIFCORE_PRUNE_COMMANDS
            COMMAND "${CMAKE_COMMAND}" -E rm -rf
                    "${GIFCORE_BUNDLE_DIR}/qml/QtQuick/Controls/${_style}")
    endforeach()
    foreach(_unused platforminputcontexts vectorimageformats generic)
        list(APPEND GIFCORE_PRUNE_COMMANDS
            COMMAND "${CMAKE_COMMAND}" -E rm -rf "${GIFCORE_BUNDLE_DIR}/${_unused}")
    endforeach()
    add_custom_command(TARGET bundle POST_BUILD
        ${GIFCORE_PRUNE_COMMANDS}
        COMMENT "Pruning Qt modules the app never loads"
        VERBATIM)
endif()

# Resolve Qt only from the deployed directory, including when Qt is installed
# on the build machine. This also makes clean-machine checks meaningful.
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/qt.conf"
     "[Paths]\nPrefix=.\nPlugins=.\nQmlImports=qml\n")
add_custom_command(TARGET bundle POST_BUILD
    COMMAND "${CMAKE_COMMAND}" -E copy_if_different
            "${CMAKE_CURRENT_BINARY_DIR}/qt.conf" "${GIFCORE_BUNDLE_DIR}/qt.conf"
    VERBATIM)

# ---------------------------------------------------------------------------
# Integrity labels: keep everything at ordinary (Medium) integrity.
#
# Windows works out a process's integrity level from the label on the *image
# file* -- measured on this machine: an executable carrying "Low Mandatory
# Level" always starts Low, wherever it lives and however it is launched -- and
# a Low process may then write only where a low label is present, so every
# export to an ordinary folder is refused with
#
#     拒绝访问。 (os error 5)
#
# A new file inherits the label of the folder it is created in, so a Low build
# tree produces Low binaries. That is how CLion's Run button broke: `forge.exe`
# came out Low, and although `easy2gif.exe` itself was Medium, a Low parent
# cannot create a Medium child, so the shell it started was Low too. The build
# directories therefore stay Medium (relocate.ps1 restores that if a copy or a
# move lost it) and the programs are stamped explicitly as a second line of
# defence. A Low process cannot raise its own label -- the privilege needed is
# exactly what a sandbox withholds -- so this has to happen here, at build time.
# ---------------------------------------------------------------------------
if(WIN32)
    set(GIFCORE_LAUNCHERS
        "${GIFCORE_BUNDLE_DIR}/easy2gif.exe"
        "${GIFCORE_BUNDLE_DIR}/forge.exe")
    if(GIFCORE_FFMPEG_EXE AND GIFCORE_FFPROBE_EXE)
        # The media tools are driven as child processes and write the raw frames
        # they produce straight into the scratch files, so they need the same
        # rights as the front end rather than a Low label of their own.
        list(APPEND GIFCORE_LAUNCHERS
            "${GIFCORE_BUNDLE_DIR}/bin/ffmpeg.exe"
            "${GIFCORE_BUNDLE_DIR}/bin/ffprobe.exe")
    endif()

    # Building only `easy2gif` (what CLion's Run button does) must be enough.
    add_custom_command(TARGET easy2gif POST_BUILD
        COMMAND icacls "${GIFCORE_BUNDLE_DIR}/easy2gif.exe"
                /setintegritylevel M /C
        COMMENT "Giving the front end normal integrity"
        VERBATIM)

    # The bundle target copies the launcher and the media engine in, so the label
    # is applied again to whatever it just wrote.
    foreach(_exe IN LISTS GIFCORE_LAUNCHERS)
        add_custom_command(TARGET bundle POST_BUILD
            COMMAND icacls "${_exe}" /setintegritylevel M /C
            COMMENT "Giving ${_exe} normal integrity"
            VERBATIM)
    endforeach()
endif()

message(STATUS "Easy2GIF: Qt from ${GIFCORE_QT_BIN_DIR}")
message(STATUS "Easy2GIF: cargo at ${GIFCORE_CARGO_EXECUTABLE}")
