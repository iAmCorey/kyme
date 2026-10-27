// 生成 Kyme 的图标，放进 crates/kyme-macos/bundle/Resources/：
//   AppIcon.icns  App 图标（Finder、系统授权弹窗）
//   kyme.tiff     菜单栏和输入法列表里的模板图（16pt，1 倍、2 倍各一张）
// 生成的文件提交进 git，平时构建用不到这个脚本。改了下面的图形之后跑一次：swift scripts/make-icons.swift
//
// 图形：一颗白色键帽，键面正中是「|<」形的 K。一竖是蓝色的输入光标，两撇是候选窗默认的高亮紫。
// 菜单栏里是同样的 K，镂空在圆角方块里。

import AppKit
import ImageIO
import UniformTypeIdentifiers

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let resources = root.appendingPathComponent("crates/kyme-macos/bundle/Resources")
let srgb = CGColorSpace(name: CGColorSpace.sRGB)!

func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
}

/// Apple 图标用的连续圆角矩形：圆角和直边之间曲率连续，不是简单的四分之一圆。
func squircle(_ rect: CGRect, radius: CGFloat) -> CGPath {
    let a: CGFloat = 1.52866483, b: CGFloat = 1.08849296, c: CGFloat = 0.86840694
    let d: CGFloat = 0.63149399, e: CGFloat = 0.07491100, f: CGFloat = 0.37282001, g: CGFloat = 0.16905599
    let r = min(radius, min(rect.width, rect.height) / 2 / a)
    // 每个角：角点、沿边进来的方向、沿边出去的方向（y 轴向上）
    let corners: [(CGPoint, CGVector, CGVector)] = [
        (CGPoint(x: rect.maxX, y: rect.maxY), CGVector(dx: 1, dy: 0), CGVector(dx: 0, dy: -1)),
        (CGPoint(x: rect.maxX, y: rect.minY), CGVector(dx: 0, dy: -1), CGVector(dx: -1, dy: 0)),
        (CGPoint(x: rect.minX, y: rect.minY), CGVector(dx: -1, dy: 0), CGVector(dx: 0, dy: 1)),
        (CGPoint(x: rect.minX, y: rect.maxY), CGVector(dx: 0, dy: 1), CGVector(dx: 1, dy: 0)),
    ]
    func at(_ o: CGPoint, _ u: CGVector, _ su: CGFloat, _ v: CGVector, _ sv: CGFloat) -> CGPoint {
        CGPoint(x: o.x + (u.dx * su + v.dx * sv) * r, y: o.y + (u.dy * su + v.dy * sv) * r)
    }
    let path = CGMutablePath()
    path.move(to: CGPoint(x: rect.minX + a * r, y: rect.maxY))
    for (o, u, v) in corners {
        path.addLine(to: at(o, u, -a, v, 0))
        path.addCurve(to: at(o, u, -d, v, e), control1: at(o, u, -b, v, 0), control2: at(o, u, -c, v, 0))
        path.addCurve(to: at(o, u, -e, v, d), control1: at(o, u, -f, v, g), control2: at(o, u, -g, v, f))
        path.addCurve(to: at(o, u, 0, v, a), control1: at(o, u, 0, v, c), control2: at(o, u, 0, v, b))
    }
    path.closeSubpath()
    return path
}

/// 圆头的折线，转成可以填充的轮廓。
func stroked(width: CGFloat, _ points: [CGPoint]) -> CGPath {
    let path = CGMutablePath()
    path.addLines(between: points)
    return path.copy(strokingWithWidth: width, lineCap: .round, lineJoin: .round, miterLimit: 10)
}

/// 「|<」形的 K：一竖是输入光标，和两撇之间留一道缝。box 是整个字的外框。
func caretK(_ box: CGRect, width w: CGFloat, gap: CGFloat) -> (stem: CGPath, arms: CGPath) {
    let stem = stroked(width: w, [CGPoint(x: box.minX + w / 2, y: box.minY + w / 2),
                                  CGPoint(x: box.minX + w / 2, y: box.maxY - w / 2)])
    let vertex = box.minX + w + gap + w / 2, end = box.maxX - w / 2
    let arms = stroked(width: w, [CGPoint(x: end, y: box.maxY - w / 2), CGPoint(x: vertex, y: box.midY),
                                  CGPoint(x: end, y: box.minY + w / 2)])
    return (stem, arms)
}

func fill(_ ctx: CGContext, _ path: CGPath, _ color: CGColor) {
    ctx.addPath(path)
    ctx.setFillColor(color)
    ctx.fillPath()
}

/// 从上到下的渐变。
func fill(_ ctx: CGContext, _ path: CGPath, top: UInt32, bottom: UInt32) {
    ctx.saveGState()
    ctx.addPath(path)
    ctx.clip()
    let box = path.boundingBoxOfPath
    let gradient = CGGradient(colorsSpace: srgb, colors: [rgb(top), rgb(bottom)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: CGPoint(x: box.midX, y: box.maxY), end: CGPoint(x: box.midX, y: box.minY),
                           options: [])
    ctx.restoreGState()
}

/// App 图标，按 macOS 的图标网格画在 1024×1024 上：824 见方的圆角方块居中，四周留出投影的位置。
/// 投影的偏移和模糊不跟着坐标缩放，要乘上 scale（实际像素和 1024 的比例）。
func drawAppIcon(_ ctx: CGContext, scale s: CGFloat) {
    let body = squircle(CGRect(x: 100, y: 100, width: 824, height: 824), radius: 185)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -10 * s), blur: 22 * s, color: rgb(0, 0.32))
    fill(ctx, body, rgb(0x4845D4))
    ctx.restoreGState()
    fill(ctx, body, top: 0x7D7BF6, bottom: 0x4845D4)

    // 键帽：浅紫的是侧边，露出来的下沿宽一些，像从斜上方看；上面是白色键面
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -16 * s), blur: 36 * s, color: rgb(0x16145E, 0.5))
    fill(ctx, squircle(CGRect(x: 262, y: 236, width: 500, height: 520), radius: 112), rgb(0xC8C6F6))
    ctx.restoreGState()
    fill(ctx, squircle(CGRect(x: 282, y: 300, width: 460, height: 436), radius: 100), top: 0xFFFFFF, bottom: 0xF2F1FF)

    let k = caretK(CGRect(x: 386, y: 384, width: 252, height: 268), width: 66, gap: 25)
    fill(ctx, k.stem, rgb(0x1AAEF2))
    fill(ctx, k.arms, rgb(0x5E5CE6))
}

/// 菜单栏图标，16×16pt：圆角方块里镂空出 K。系统只看不透明度，颜色跟着菜单栏走。
func drawMenuIcon(_ ctx: CGContext) {
    fill(ctx, squircle(CGRect(x: 0.5, y: 0.5, width: 15, height: 15), radius: 3.3), rgb(0))
    let k = caretK(CGRect(x: 4.2, y: 3.6, width: 7.8, height: 8.8), width: 2.1, gap: 1.0)
    ctx.setBlendMode(.clear)
    fill(ctx, k.stem, rgb(0))
    fill(ctx, k.arms, rgb(0))
}

/// 画一张 pixels 见方的图，图形按 points 见方的坐标来画。
func render(pixels: Int, points: CGFloat, _ draw: (CGContext, CGFloat) -> Void) -> CGImage {
    let ctx = CGContext(data: nil, width: pixels, height: pixels, bitsPerComponent: 8, bytesPerRow: 0, space: srgb,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let scale = CGFloat(pixels) / points
    ctx.scaleBy(x: scale, y: scale)
    draw(ctx, scale)
    return ctx.makeImage()!
}

func write(_ images: [(CGImage, [CFString: Any])], to url: URL, type: UTType) {
    let destination = CGImageDestinationCreateWithURL(url as CFURL, type.identifier as CFString, images.count, nil)!
    for (image, properties) in images {
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
    }
    guard CGImageDestinationFinalize(destination) else { fatalError("写不出 \(url.path)") }
}

// App 图标：每个尺寸都按矢量重新画一遍（小尺寸不是大图缩出来的），再用 iconutil 合成 .icns
let iconset = FileManager.default.temporaryDirectory.appendingPathComponent("kyme-\(getpid()).iconset")
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for size in [16, 32, 128, 256, 512] {
    for factor in [1, 2] {
        let name = factor == 1 ? "icon_\(size)x\(size).png" : "icon_\(size)x\(size)@2x.png"
        let image = render(pixels: size * factor, points: 1024) { drawAppIcon($0, scale: $1) }
        write([(image, [:])], to: iconset.appendingPathComponent(name), type: .png)
    }
}
let icns = resources.appendingPathComponent("AppIcon.icns")
let iconutil = Process()
iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
iconutil.arguments = ["-c", "icns", iconset.path, "-o", icns.path]
try iconutil.run()
iconutil.waitUntilExit()
try? FileManager.default.removeItem(at: iconset)
guard iconutil.terminationStatus == 0 else { fatalError("iconutil 合成 \(icns.path) 失败") }

// 菜单栏图标：1 倍、2 倍放进同一个 TIFF，系统按屏幕挑
func dpi(_ value: Int) -> [CFString: Any] { [kCGImagePropertyDPIWidth: value, kCGImagePropertyDPIHeight: value] }
let tiff = resources.appendingPathComponent("kyme.tiff")
write([(render(pixels: 16, points: 16) { ctx, _ in drawMenuIcon(ctx) }, dpi(72)),
       (render(pixels: 32, points: 16) { ctx, _ in drawMenuIcon(ctx) }, dpi(144))], to: tiff, type: .tiff)
print("已生成 \(icns.path)\n已生成 \(tiff.path)")
