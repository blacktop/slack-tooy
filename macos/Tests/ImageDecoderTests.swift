import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers
import XCTest

final class ImageDecoderTests: XCTestCase {
    private static let decoder = ImageDecode()
    private func encodedImage(type: UTType, width: Int, height: Int, alpha: CGFloat = 1) throws -> Data {
        let colorSpace = try XCTUnwrap(CGColorSpace(name: CGColorSpace.sRGB))
        let context = try XCTUnwrap(CGContext(
            data: nil, width: width, height: height,
            bitsPerComponent: 8, bytesPerRow: width * 4, space: colorSpace,
            bitmapInfo: CGBitmapInfo.byteOrder32Big.rawValue | CGImageAlphaInfo.premultipliedLast.rawValue
        ))
        context.setFillColor(try XCTUnwrap(CGColor(colorSpace: colorSpace, components: [1, 0, 0, alpha])))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        let image = try XCTUnwrap(context.makeImage())
        let data = NSMutableData()
        let destination = try XCTUnwrap(CGImageDestinationCreateWithData(data, type.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(destination, image, nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        return data as Data
    }

    func testDecodesJPEGAndPNGAsStraightRGBA() throws {
        let png = try XCTUnwrap(Self.decoder?.decode(try encodedImage(type: .png, width: 1, height: 1, alpha: 0.5)))
        XCTAssertEqual(png.width, 1)
        XCTAssertEqual(png.height, 1)
        XCTAssertEqual(Array(png.rgba), [255, 0, 0, 128])

        let jpeg = try XCTUnwrap(Self.decoder?.decode(try encodedImage(type: .jpeg, width: 3, height: 2)))
        XCTAssertEqual(jpeg.width, 3)
        XCTAssertEqual(jpeg.height, 2)
        XCTAssertEqual(jpeg.rgba.count, 24)
        XCTAssertEqual(jpeg.rgba[3], 255)
    }

    func testRejectsMalformedOversizedAndUnsupportedData() throws {
        XCTAssertNil(Self.decoder?.decode(Data()))
        XCTAssertNil(Self.decoder?.decode(Data("not an image".utf8)))
        XCTAssertNil(Self.decoder?.decode(Data(repeating: 0, count: MediaLimits.encodedBytes + 1)))
        XCTAssertNil(Self.decoder?.decode(try encodedImage(type: .tiff, width: 1, height: 1)))
    }

    func testLargeImageProducesBoundedThumbnail() throws {
        let response = try XCTUnwrap(Self.decoder?.decode(try encodedImage(type: .png, width: 4096, height: 2048)))
        XCTAssertEqual(response.width, 2048)
        XCTAssertEqual(response.height, 1024)
        XCTAssertEqual(response.rgba.count, 2048 * 1024 * 4)
    }

    func testRejectsExcessiveOriginalDimensionsBeforeThumbnailing() throws {
        let encoded = try encodedImage(type: .png, width: MediaLimits.sourceDimension + 1, height: 1)
        let source = try XCTUnwrap(CGImageSourceCreateWithData(encoded as CFData, nil))
        let options = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceThumbnailMaxPixelSize: 2048,
        ] as CFDictionary
        XCTAssertNotNil(CGImageSourceCreateThumbnailAtIndex(source, 0, options))
        XCTAssertNil(Self.decoder?.decode(encoded))
    }
}
