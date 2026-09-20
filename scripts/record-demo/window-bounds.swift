// Geometry the video post-processing needs, printed as plain numbers.
//
//   swift window-bounds.swift <pid>    -> x y width height   (points, top-left origin)
//   swift window-bounds.swift display  -> width height scale (main display, points)
//   swift window-bounds.swift activate <pid>  -> bring that app to the front
//   swift window-bounds.swift park-pointer    -> move the pointer to the screen's
//                                                bottom-right corner, out of frame
//
// Window bounds are readable without the Screen Recording permission; only
// window titles are withheld, which is why this matches on the window-owner pid.
import AppKit
import CoreGraphics
import Foundation

if CommandLine.arguments.count == 3, CommandLine.arguments[1] == "activate",
   let pid = Int32(CommandLine.arguments[2]),
   let app = NSRunningApplication(processIdentifier: pid) {
    exit(app.activate() ? 0 : 1)
}

if CommandLine.arguments.count == 2, CommandLine.arguments[1] == "park-pointer" {
    guard let screen = NSScreen.screens.first else { exit(1) }
    // Moving the pointer is not posting an input event, so it needs no
    // Accessibility permission.
    let corner = CGPoint(x: screen.frame.width - 2, y: screen.frame.height - 2)
    exit(CGWarpMouseCursorPosition(corner) == .success ? 0 : 1)
}

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write("usage: window-bounds.swift <pid>|display\n".data(using: .utf8)!)
    exit(2)
}

if CommandLine.arguments[1] == "display" {
    guard let screen = NSScreen.screens.first else { exit(1) }
    print(Int(screen.frame.width), Int(screen.frame.height), screen.backingScaleFactor)
    exit(0)
}

guard let pid = Int(CommandLine.arguments[1]) else { exit(2) }

let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]] ?? []

var best: CGRect?
for window in windows {
    guard (window[kCGWindowOwnerPID as String] as? Int) == pid,
          (window[kCGWindowLayer as String] as? Int) == 0,
          let dict = window[kCGWindowBounds as String] as? NSDictionary,
          let rect = CGRect(dictionaryRepresentation: dict)
    else { continue }
    if best == nil || rect.width * rect.height > best!.width * best!.height {
        best = rect
    }
}

guard let rect = best else { exit(1) }
print(Int(rect.origin.x), Int(rect.origin.y), Int(rect.width), Int(rect.height))
