import Darwin

@main
enum SandboxProbe {
    static func main() {
        do { try SandboxLaunch.execIfRequested() } catch { exit(2) }
        let arguments = CommandLine.arguments
        guard arguments.count == 3 else { exit(2) }
        if arguments[1] == "read" || arguments[1] == "fd" {
            let descriptor = arguments[1] == "read"
                ? open(arguments[2], O_RDONLY)
                : (Int32(arguments[2]) ?? -1)
            guard descriptor >= 0 else {
                print("denied \(errno)")
                exit(1)
            }
            defer { close(descriptor) }
            var byte: UInt8 = 0
            guard Darwin.read(descriptor, &byte, 1) == 1 else {
                print("denied \(errno)")
                exit(1)
            }
            print("permitted")
            return
        }
        guard arguments[1] == "connect", let port = UInt16(arguments[2]) else { exit(2) }
        let descriptor = socket(AF_INET, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            print("denied \(errno)")
            exit(1)
        }
        defer { close(descriptor) }
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        let result = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        if result != 0 {
            print("denied \(errno)")
            exit(1)
        }
        print("permitted")
    }
}
