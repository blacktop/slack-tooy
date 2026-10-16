import Darwin
import Foundation

@main
enum LocalImageDecoder {
    static func main() {
        do {
            try SandboxLaunch.execIfRequested()
            guard CommandLine.arguments.count == 1,
                  let decoder = ImageDecode(),
                  let response = decoder.decode(try ImageWire.readInput()) else {
                throw ImageWireError.invalidInput
            }
            try ImageWire.writeResponse(response)
        } catch {
            try? FileHandle.standardError.write(contentsOf: Data("Image decoder rejected the request\n".utf8))
            exit(1)
        }
    }
}
