import Foundation

enum MediaLimits {
    static let encodedBytes = 16 * 1024 * 1024
    static let sourceDimension = 16_384
    static let sourcePixels = 64_000_000
    static let dimension = 4096
    static let decodedBytes = 16 * 1024 * 1024
}

struct DecodedImage {
    let width: UInt32
    let height: UInt32
    let rgba: Data
}
