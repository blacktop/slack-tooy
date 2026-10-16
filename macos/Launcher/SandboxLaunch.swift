import Darwin

enum SandboxLaunchError: Error {
    case system(Int32)
}

enum SandboxLaunch {
    static func execIfRequested() throws {
        let arguments = CommandLine.arguments
        guard arguments.count > 1, arguments[1] == "--sandbox" else { return }
        guard arguments.count >= 3, arguments[0].hasPrefix("/") else {
            throw SandboxLaunchError.system(EINVAL)
        }

        var attributes: posix_spawnattr_t?
        try check(posix_spawnattr_init(&attributes))
        defer { posix_spawnattr_destroy(&attributes) }
        try check(posix_spawnattr_setflags(
            &attributes, Int16(POSIX_SPAWN_SETEXEC | POSIX_SPAWN_CLOEXEC_DEFAULT)
        ))

        var actions: posix_spawn_file_actions_t?
        try check(posix_spawn_file_actions_init(&actions))
        defer { posix_spawn_file_actions_destroy(&actions) }
        for descriptor in STDIN_FILENO...STDERR_FILENO {
            try check(posix_spawn_file_actions_adddup2(&actions, descriptor, descriptor))
        }

        let command = ["/usr/bin/sandbox-exec", "-D", "HELPER=\(arguments[0])",
                       "-p", arguments[2], arguments[0]] + arguments.dropFirst(3)
        var argv: [UnsafeMutablePointer<CChar>?] = []
        defer { for argument in argv { free(argument) } }
        for argument in command {
            guard let copy = strdup(argument) else { throw SandboxLaunchError.system(ENOMEM) }
            argv.append(copy)
        }
        argv.append(nil)
        let status = argv.withUnsafeBufferPointer { buffer in
            guard let base = buffer.baseAddress else { return EINVAL }
            var environment: UnsafeMutablePointer<CChar>?
            return posix_spawn(nil, "/usr/bin/sandbox-exec", &actions, &attributes, base, &environment)
        }
        // SETEXEC succeeds by replacing this process; never decode after a failed launch.
        throw SandboxLaunchError.system(status == 0 ? EIO : status)
    }

    private static func check(_ status: Int32) throws {
        guard status == 0 else { throw SandboxLaunchError.system(status) }
    }
}
