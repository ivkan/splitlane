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

// hover <x> <y>: the pointer moves there as a person's would, two short
// steps ending on the point, so the app sees mouse-moved events and starts
// its hover and tooltip. Unlike the rest of this file, posting input events
// needs the Accessibility (PostEvent) permission for the app the script runs
// from. The caller keeps the point inside the demo window, in front.
if CommandLine.arguments.count == 4, CommandLine.arguments[1] == "hover",
   let x = Double(CommandLine.arguments[2]), let y = Double(CommandLine.arguments[3]) {
    guard CGPreflightPostEventAccess() else {
        FileHandle.standardError.write("hover: no permission to post input events (Accessibility)\n".data(using: .utf8)!)
        exit(1)
    }
    for (dx, dy) in [(-6.0, -2.0), (-3.0, -1.0), (0.0, 0.0)] {
        let point = CGPoint(x: x + dx, y: y + dy)
        CGWarpMouseCursorPosition(point)
        CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: point, mouseButton: .left)?
            .post(tap: .cghidEventTap)
        usleep(60_000)
    }
    exit(0)
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
