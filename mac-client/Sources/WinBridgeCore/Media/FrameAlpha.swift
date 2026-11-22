import CoreGraphics
import CoreVideo
import Foundation
import ImageIO
import VideoToolbox

/// Self-contained, lossless alpha paired with one color frame, including dropped frames.
struct FrameAlpha {
    let width: Int
    let height: Int
    let values: [UInt8]
    enum Invalid: Error { case envelope, mask, dimensions }

    static func unpack(_ frame: MediaFrame) throws -> (Data, FrameAlpha?) {
        guard frame.header.flags.contains(.alphaPNG) else { return (frame.payload, nil) }
        let w=Int(frame.header.width), h=Int(frame.header.height)
        guard w>0, h>0, w*h<=16_777_216, frame.payload.count>=5 else {throw Invalid.envelope}
        let count=frame.payload.prefix(4).reduce(0) { ($0<<8)|Int($1) }
        guard count>0, count<frame.payload.count-4,
              frame.payload.count-4-count<=8*1024*1024 else {throw Invalid.envelope}
        let color=frame.payload.subdata(in:4..<(4+count))
        let data=frame.payload.subdata(in:(4+count)..<frame.payload.count)
        guard let source=CGImageSourceCreateWithData(data as CFData,nil),
              CGImageSourceGetType(source) as String? == "public.png",
              let props=CGImageSourceCopyPropertiesAtIndex(source,0,nil) as? [CFString:Any],
              (props[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue==w,
              (props[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue==h else {throw Invalid.dimensions}
        guard let mask=CGImageSourceCreateImageAtIndex(source,0,nil),mask.bitsPerPixel==8,
              mask.bitsPerComponent==8,mask.colorSpace?.numberOfComponents==1,
              mask.alphaInfo == .none,mask.bytesPerRow>=w,
              let bytes=mask.dataProvider?.data as Data?,bytes.count>=mask.bytesPerRow*h else {throw Invalid.mask}
        var values=[UInt8]();values.reserveCapacity(w*h)
        for row in 0..<h {values.append(contentsOf:bytes[(row*mask.bytesPerRow)..<(row*mask.bytesPerRow+w)])}
        return (color,FrameAlpha(width:w,height:h,values:values))
    }

    func composite(_ color: DecodedImage) -> CGImage? {
        let image:CGImage
        switch color {
        case .cgImage(let cg):image=cg
        case .pixelBuffer(let pb):
            var cg:CGImage?
            guard VTCreateCGImageFromCVPixelBuffer(pb,options:nil,imageOut:&cg)==noErr,let cg else{return nil}
            image=cg
        }
        guard image.width==width,image.height==height,values.count==width*height else{return nil}
        let layout=CGBitmapInfo.byteOrder32Little.rawValue | CGImageAlphaInfo.premultipliedFirst.rawValue
        var pixels=[UInt8](repeating:0,count:width*height*4)
        let drawn=pixels.withUnsafeMutableBytes { raw -> Bool in
            guard let context=CGContext(data:raw.baseAddress,width:width,height:height,bitsPerComponent:8,
                bytesPerRow:width*4,space:CGColorSpaceCreateDeviceRGB(),bitmapInfo:layout) else{return false}
            context.setBlendMode(.copy);context.draw(image,in:CGRect(x:0,y:0,width:width,height:height));return true
        }
        guard drawn else{return nil}
        for i in values.indices {
            let a=values[i]
            // WGC color is premultiplied. Replace alpha; do not multiply RGB a
            // second time (which would produce dark halos on translucent shadows).
            for c in 0..<3 {pixels[i*4+c]=min(pixels[i*4+c],a)}
            pixels[i*4+3]=a
        }
        guard let provider=CGDataProvider(data:Data(pixels) as CFData) else{return nil}
        return CGImage(width:width,height:height,bitsPerComponent:8,bitsPerPixel:32,bytesPerRow:width*4,
            space:CGColorSpaceCreateDeviceRGB(),bitmapInfo:CGBitmapInfo(rawValue:layout),provider:provider,
            decode:nil,shouldInterpolate:true,intent:.defaultIntent)
    }
}
