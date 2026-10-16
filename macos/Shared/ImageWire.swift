import Foundation

enum ImageWireError: Error {
    case invalidInput
    case invalidResponse
}

enum ImageWire {
    private static func readExactly(_ count: Int) throws -> Data {
        var bytes = Data()
        bytes.reserveCapacity(count)
        while bytes.count < count {
            guard let chunk = try FileHandle.standardInput.read(upToCount: count - bytes.count),
                  !chunk.isEmpty else {
                throw ImageWireError.invalidInput
            }
            bytes.append(chunk)
        }
        return bytes
    }

    static func readInput() throws -> Data {
        let header = try readExactly(4)
        let count = header.enumerated().reduce(UInt32(0)) {
            $0 | UInt32($1.element) << ($1.offset * 8)
        }
        guard count > 0, count <= MediaLimits.encodedBytes else {
            throw ImageWireError.invalidInput
        }
        return try readExactly(Int(count))
    }

    static func writeResponse(_ response: DecodedImage) throws {
        let width = Int(response.width)
        let height = Int(response.height)
        guard width > 0, height > 0,
              width <= MediaLimits.dimension, height <= MediaLimits.dimension,
              width * height <= MediaLimits.decodedBytes / 4,
              response.rgba.count == width * height * 4 else {
            throw ImageWireError.invalidResponse
        }
        var header = Data("STIM".utf8)
        for value in [response.width, response.height, UInt32(response.rgba.count)] {
            var littleEndian = value.littleEndian
            withUnsafeBytes(of: &littleEndian) { header.append(contentsOf: $0) }
        }
        try FileHandle.standardOutput.write(contentsOf: header)
        try FileHandle.standardOutput.write(contentsOf: response.rgba)
    }
}
