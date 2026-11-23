import AppKit
import CoreGraphics
import Foundation
import ImageIO
import Testing
@testable import WinBridgeCore

private func maskPNG(_ bytes:[UInt8],width:Int,height:Int)throws->Data {
    let provider=try #require(CGDataProvider(data:Data(bytes) as CFData))
    let image=try #require(CGImage(width:width,height:height,bitsPerComponent:8,bitsPerPixel:8,
        bytesPerRow:width,space:CGColorSpaceCreateDeviceGray(),bitmapInfo:CGBitmapInfo(rawValue:0),
        provider:provider,decode:nil,shouldInterpolate:false,intent:.defaultIntent))
    let data=NSMutableData()
    let destination=try #require(CGImageDestinationCreateWithData(data,"public.png" as CFString,1,nil))
    CGImageDestinationAddImage(destination,image,nil)
    #expect(CGImageDestinationFinalize(destination));return data as Data
}
private func alphaFrame(color:Data,mask:Data,width:UInt16=2,height:UInt16=2)->MediaFrame {
    var count=UInt32(color.count).bigEndian
    var bytes=withUnsafeBytes(of:&count){Data($0)};bytes.append(color);bytes.append(mask)
    return MediaFrame(header:MediaFrameHeader(windowId:1,timestampUs:0,width:width,height:height,
        codec:.rawBgra,flags:[.keyframe,.alphaPNG],frameSeq:0,payloadLen:UInt32(bytes.count)),payload:bytes)
}

@Test func alphaEnvelopeRejectsMismatchedDimensionsAndTruncation()throws {
    let frame=alphaFrame(color:Data([1,2]),mask:try maskPNG([255,128,0,255],width:2,height:2))
    let (color,mask)=try FrameAlpha.unpack(frame)
    #expect(color==Data([1,2]));#expect(mask?.values==[255,128,0,255])
    let wrong=alphaFrame(color:Data([1]),mask:try maskPNG([255],width:1,height:1))
    #expect(throws:(any Error).self) {try FrameAlpha.unpack(wrong)}
    for bytes in [Data([0,0,0,0,1]),Data([255,255,255,255,1]),Data([0,0,0,1,9]),Data()] {
        var header=frame.header;header.payloadLen=UInt32(bytes.count)
        #expect(throws:(any Error).self) {try FrameAlpha.unpack(MediaFrame(header:header,payload:bytes))}
    }
    var plain=frame.header;plain.flags=[.keyframe]
    #expect(try FrameAlpha.unpack(MediaFrame(header:plain,payload:Data([7]))).1==nil)
}

@Test func alphaDecoderPreservesBlackContentAndPremultipliedEdges() async throws {
    let mask=try maskPNG([255,128,0,255],width:2,height:2)
    let color=Data([0,0,0,255, 64,32,16,255, 0,0,0,255, 90,80,70,255])
    let decoder=WindowVideoDecoder(windowId:1)
    let image:CGImage=await withCheckedContinuation { cont in
        decoder.onDecodedFrame={frame,_,_ in if case .cgImage(let image)=frame {cont.resume(returning:image)}}
        decoder.decode(alphaFrame(color:color,mask:mask))
    }
    let pixels=try #require(image.dataProvider?.data as Data?)
    #expect(image.alphaInfo == .premultipliedFirst)
    #expect(Array(pixels)==[0,0,0,255, 64,32,16,128, 0,0,0,0, 90,80,70,255])
    // No cached mask may survive an opaque frame or a different window/size.
    let opaque:CGImage=await withCheckedContinuation { cont in
        decoder.onDecodedFrame={frame,_,_ in if case .cgImage(let image)=frame {cont.resume(returning:image)}}
        let header=MediaFrameHeader(windowId:1,timestampUs:1,width:2,height:2,codec:.rawBgra,
            flags:[.keyframe],frameSeq:1,payloadLen:UInt32(color.count))
        decoder.decode(MediaFrame(header:header,payload:color))
    }
    #expect((opaque.dataProvider?.data as Data?)==color)
}

@MainActor @Test func followedImageViewerHasNativeFrameAndTransparentCanvas() {
    _=NSApplication.shared
    let owner=RemoteWindowController(info:WindowInfo(windowId:1,pid:7,kind:.normal,width:640,height:480,dpi:96),owner:nil)
    let image=RemoteWindowController(info:WindowInfo(windowId:2,ownerWindowId:1,pid:7,kind:.dialog,width:320,height:240,dpi:144),owner:owner)
    #expect(image.window?.styleMask.contains(.titled)==true)
    #expect(image.window?.canBecomeKey==true)
    image.applyTransparency(true)
    #expect(image.window?.isOpaque==true)
    #expect(image.window?.hasShadow==true)
    #expect(image.window?.backgroundColor == .windowBackgroundColor)
    #expect(image.window?.titlebarAppearsTransparent == false)
    #expect(image.remoteView.layer?.backgroundColor?.alpha==0)
    image.applyTransparency(false)
    #expect(image.window?.isOpaque==true)
    image.forceCloseLocally();owner.forceCloseLocally()
}

@Test(.enabled(if: ProcessInfo.processInfo.environment["WINBRIDGE_TEST_FRAME_OUT"] != nil))
func capturedWindowsH264AlphaFrameDecodes() async throws {
    guard let path=ProcessInfo.processInfo.environment["WINBRIDGE_TEST_FRAME_OUT"] else{return}
    let data=try Data(contentsOf:URL(fileURLWithPath:path))
    #expect(data.count>4)
    let w=UInt16(data[0])<<8|UInt16(data[1]), h=UInt16(data[2])<<8|UInt16(data[3])
    let payload=data.subdata(in:4..<data.count)
    let frame=MediaFrame(header:MediaFrameHeader(windowId:1,timestampUs:0,width:w,height:h,codec:.h264AnnexB,
        flags:[.keyframe,.alphaPNG],frameSeq:0,payloadLen:UInt32(payload.count)),payload:payload)
    let (_,alpha)=try FrameAlpha.unpack(frame)
    let expected=try #require(alpha)
    let decoder=WindowVideoDecoder(windowId:1)
    let image:CGImage=await withCheckedContinuation { cont in
        decoder.onDecodedFrame={decoded,_,_ in if case .cgImage(let image)=decoded {cont.resume(returning:image)}}
        decoder.decode(frame)
    }
    let pixels=try #require(image.dataProvider?.data as Data?)
    #expect(image.width==Int(w) && image.height==Int(h))
    #expect(expected.values.contains(0) && expected.values.contains(128) && expected.values.contains(255))
    for y in 0..<Int(h) {for x in 0..<Int(w) {
        #expect(pixels[y*image.bytesPerRow+x*4+3]==expected.values[y*Int(w)+x])
    }}
    // The native fixture's right-hand shadow is premultiplied white at alpha 128.
    // A second alpha multiplication would darken these channels to about 64.
    let shadow=12*image.bytesPerRow+96*4
    #expect(pixels[shadow+3]==128)
    for c in 0..<3 {#expect((110...128).contains(pixels[shadow+c]))}
    let black=48*image.bytesPerRow+32*4
    #expect(pixels[black+3]==255)
    for c in 0..<3 {#expect(pixels[black+c]<=5)}
    print("PASS: actual Windows WGC/H.264 color + grayscale alpha decoded and composited on Mac")
}
