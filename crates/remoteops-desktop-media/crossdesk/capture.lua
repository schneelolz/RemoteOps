add_requires("libyuv 2025.8.14", "spdlog 1.14.1", {system = false})
target("remoteops-crossdesk-native")
    set_kind("shared")
    set_languages("c++17")
    add_defines("NOMINMAX", "WIN32_LEAN_AND_MEAN", "UNICODE", "_UNICODE")
    add_cxxflags("/FIole2.h", {tools = "cl", force = true})
    add_files("capture.cpp","uia.cpp","../apps/desktop/src/common/desktop_automation.cpp", "../apps/desktop/src/platform/windows/screen_capturer/screen_capturer_gdi.cpp")
    add_includedirs("../apps/desktop/src/screen_capturer", "../apps/desktop/src/platform/windows/screen_capturer",
                    "../apps/desktop/src/platform/windows/service", "../apps/desktop/src/log",
                    "../apps/desktop/src/platform/windows/input", "../apps/desktop/src/common", "../deps/submodules/minirtc/src/api")
    add_deps("crossdesk_controller")
    add_packages("libyuv", "spdlog", "nlohmann_json")
    add_syslinks("user32", "gdi32", "wtsapi32","uiautomationcore","ole32","oleaut32","shell32","uuid")
target_end()

target("remoteops-crossdesk-desktop-fixture")
    set_kind("binary")
    set_languages("c++17")
    add_cxxflags("/FIole2.h", {tools = "cl", force = true})
    add_defines("NOMINMAX", "WIN32_LEAN_AND_MEAN", "UNICODE", "_UNICODE")
    add_files("desktop_fixture.cpp", "../apps/desktop/src/common/desktop_automation.cpp",
              "../apps/desktop/src/platform/windows/desktop_automation.cpp",
              "../apps/desktop/src/platform/windows/input/windows_input_injector.cpp",
              "../apps/desktop/src/log/rd_log.cpp")
    add_includedirs("../apps/desktop/src/common","../apps/desktop/src/platform/windows/service",
                    "../apps/desktop/src/platform/windows/input","../apps/desktop/src/log")
    add_deps("remoteops-crossdesk-native")
    add_packages("nlohmann_json", "spdlog")
    add_syslinks("uiautomationcore","ole32","oleaut32","user32","gdi32","wtsapi32","shell32","uuid")
target_end()
