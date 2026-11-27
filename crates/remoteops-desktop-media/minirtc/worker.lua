target("remoteops-minirtc")
    set_kind("binary")
    add_deps("minirtc", "pc", "transport", "ice", "media", "log", "ws", "common")
    add_files("remoteops_worker.cpp")
    add_syslinks("advapi32", "dnsapi", "shlwapi", "crypt32", "ws2_32",
                 "user32", "strmiids", "mfuuid", "mfplat", "mf", "secur32",
                 "bcrypt", "shell32", "ole32", "iphlpapi")
