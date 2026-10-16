import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

struct ImageDecode: Sendable {
    init?() {
        let types = [UTType.jpeg.identifier, UTType.png.identifier] as CFArray
        guard CGImageSourceSetAllowableTypes(types) == 0 else { return nil }
    }

    func decode(_ encoded: Data) -> DecodedImage? {
        guard !encoded.isEmpty, encoded.count <= MediaLimits.encodedBytes else { return nil }
        let sourceOptions = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithData(encoded as CFData, sourceOptions),
              let type = CGImageSourceGetType(source),
              CFEqual(type, UTType.jpeg.identifier as CFString) || CFEqual(type, UTType.png.identifier as CFString) else {
            return nil
        }
        guard let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, sourceOptions) as? [CFString: Any],
              let sourceWidth = properties[kCGImagePropertyPixelWidth] as? Int,
              let sourceHeight = properties[kCGImagePropertyPixelHeight] as? Int,
              sourceWidth > 0, sourceHeight > 0,
              sourceWidth <= MediaLimits.sourceDimension,
              sourceHeight <= MediaLimits.sourceDimension else { return nil }
        let sourcePixels = sourceWidth.multipliedReportingOverflow(by: sourceHeight)
        guard !sourcePixels.overflow, sourcePixels.partialValue <= MediaLimits.sourcePixels else { return nil }
        // 2048 square pixels keep even the largest thumbnail inside 16 MiB.
        let options: CFDictionary = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: 2048,
        ] as CFDictionary
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options) else { return nil }
        let width = image.width
        let height = image.height
        guard width > 0, height > 0, width <= MediaLimits.dimension, height <= MediaLimits.dimension,
              width * height <= MediaLimits.decodedBytes / 4 else { return nil }
        guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB) else { return nil }
        var pixels = Data(count: width * height * 4)
        let rendered = pixels.withUnsafeMutableBytes { storage in
            guard let context = CGContext(
                data: storage.baseAddress, width: width, height: height,
                bitsPerComponent: 8, bytesPerRow: width * 4,
                space: colorSpace,
                bitmapInfo: CGBitmapInfo.byteOrder32Big.rawValue | CGImageAlphaInfo.premultipliedLast.rawValue
            ) else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        guard rendered else { return nil }
        pixels.withUnsafeMutableBytes { storage in
            let bytes = storage.bindMemory(to: UInt8.self)
            for pixel in stride(from: 0, to: bytes.count, by: 4) {
                let alpha = UInt32(bytes[pixel + 3])
                for channel in 0..<3 {
                    bytes[pixel + channel] = alpha == 0 ? 0 : UInt8(min(255, (UInt32(bytes[pixel + channel]) * 255 + alpha / 2) / alpha))
                }
            }
        }
        return DecodedImage(width: UInt32(width), height: UInt32(height), rgba: pixels)
    }
}
