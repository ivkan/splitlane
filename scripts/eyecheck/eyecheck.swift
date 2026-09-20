// Eye-check helper: drive the running Splitlane window from a script.
//
// macOS only, and deliberately so - it exists to serve checking the UI by eye
// (raise the window, screenshot it, look), which is a macOS procedure. Nothing
// in the product depends on it.
//
// RAISE FIRST, THEN LOOK. THAT ORDER IS THE WHOLE TRICK.
//
// `CGWindowListCopyWindowInfo([.optionOnScreenOnly], ...)` does not list the
// Splitlane window while the app is not on screen, and a helper that looks the
// window up *before* activating the app therefore reports "no splitlane window"
// intermittently - working right after something else raised it, failing a
// minute later. Every subcommand here activates and confirms frontmost first,
// and only then enumerates.
//
// Diagnosed 27 August 2026, after first blaming the wrong thing: the failures
// looked exactly like a missing Screen Recording grant, and Screen Recording
// *is* granted per executable path, so freshly built throwaway helpers made a
// convincing story. It was false. `.optionAll` saw the window from the same
// binary at the same moment, and both the old and the new binary started
// working the instant the app was raised. Kept as a warning: a permission
// failure and an off-screen window are indistinguishable from the return value.
//
// SUBCOMMANDS
//   window                  print: <id> <x> <y> <w> <h>   (screen coordinates)
//
// Set SPLITLANE_EYECHECK_WINDOW=<id> when a dev build and the installed app are
// both on screen; without it every subcommand refuses rather than guess.
//   click <x> <y>           left click at window-relative point
//   dclick <x> <y>          double click (the inline-rename gesture)
//   drag <x1> <y1> <x2> <y2>  press, move in steps, release
//   type <text>             literal text, layout-independent
//   key <code> [unichar]    one key by virtual code, with an optional character
//   enter | esc | shifttab  the three this procedure needs by name
//   chord <mods> <code>     a modified key: mods from cmd,shift,alt,ctrl
//
// Every command that synthesises input raises Splitlane and then RE-CHECKS that
// it is frontmost before posting anything, and aborts if it is not. That check
// is inside the helper on purpose: focus can return to the editor between an
// `osascript` activate and the next shell command, and a chord addressed to
// Splitlane then lands in the user's editor. Measured, and it cost a session.

import AppKit
import CoreGraphics
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("\(message)\n".utf8))
    exit(1)
}

/// The Splitlane window's screen rect, or nil when the window server will not
/// name it for us (see the note above about Screen Recording).
///
/// TWO SPLITLANES IS THE DANGEROUS CASE, AND IT IS THE ORDINARY ONE.
///
/// A `cargo run` build and the installed app are fully isolated on disk and
/// deliberately run side by side, so during any UI check there are usually two
/// windows a "splitlane" owner-name match accepts. This used to take whichever
/// the window server happened to enumerate first, and the enumeration order
/// changes between calls: on 29 August a right-click meant for the dev build
/// opened a context menu in the live app instead, over a running agent
/// session, while the screenshot - taken by explicit window id - showed the
/// dev window with nothing on it. Input went one way and the eyes went the
/// other, which is the one failure this helper must not have.
///
/// So ambiguity is refused rather than resolved by luck. Name the window with
/// `SPLITLANE_EYECHECK_WINDOW=<id>` (the id `window` prints) to work with one
/// of two, or quit the other.
func splitlaneWindow() -> (id: Int, pid: pid_t, rect: CGRect)? {
    let info =
        CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID)
        as? [[String: Any]] ?? []
    let wanted = ProcessInfo.processInfo.environment["SPLITLANE_EYECHECK_WINDOW"].flatMap { Int($0) }
    var found: [(id: Int, pid: pid_t, rect: CGRect)] = []
    for entry in info {
        let owner = entry[kCGWindowOwnerName as String] as? String ?? ""
        guard owner.lowercased().contains("splitlane") else { continue }
        let bounds = entry[kCGWindowBounds as String] as? [String: Any] ?? [:]
        let height = bounds["Height"] as? Double ?? 0
        // The app also owns several 1728x33 strips and a 500x500; the document
        // window is the tall one.
        guard height > 200 else { continue }
        let id = entry[kCGWindowNumber as String] as? Int ?? 0
        if let wanted, id != wanted { continue }
        found.append(
            (
                id,
                entry[kCGWindowOwnerPID as String] as? pid_t ?? 0,
                CGRect(
                    x: bounds["X"] as? Double ?? 0, y: bounds["Y"] as? Double ?? 0,
                    width: bounds["Width"] as? Double ?? 0, height: height)
            ))
    }
    if found.count > 1 {
        // With their geometry, because that is how you tell a dev build from
        // the installed app without clicking either of them.
        let ids = found.map {
            "\($0.id) at \(Int($0.rect.origin.x)),\(Int($0.rect.origin.y))"
                + " \(Int($0.rect.width))x\(Int($0.rect.height))"
        }.joined(separator: "; ")
        fail(
            """
            ABORT: \(found.count) splitlane windows are on screen (\(ids)).
            Input and screenshots would not agree on which one they mean - a
            click could land in the app you are not looking at. Quit one, or
            name the one you mean:

              SPLITLANE_EYECHECK_WINDOW=\(found[0].id) scripts/eyecheck/eyecheck.sh ...
            """)
    }
    return found.first
}

/// Ask System Events to bring one process forward, by pid. Best effort: any
/// failure just leaves the frontmost check below to refuse.
///
/// By pid and not by name, because two splitlanes answer to the name and
/// System Events picks one of them - which is the same coin toss the window
/// lookup refuses to make.
func raiseViaSystemEvents(pid: pid_t) {
    let task = Process()
    task.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
    task.arguments = [
        "-e",
        "tell application \"System Events\" to set frontmost of "
            + "(first process whose unix id is \(pid)) to true",
    ]
    task.standardError = FileHandle.nullDevice
    task.standardOutput = FileHandle.nullDevice
    try? task.run()
    task.waitUntilExit()
}

/// Every running application whose name says splitlane.
func splitlaneApps() -> [NSRunningApplication] {
    NSWorkspace.shared.runningApplications.filter {
        ($0.localizedName ?? "").lowercased().contains("splitlane")
    }
}

/// Raise Splitlane and confirm it actually came forward. Returns the window rect.
func focusSplitlane() -> CGRect {
    // RAISE FIRST, THEN LOOK - see the note at the top of this file. An
    // off-screen app's window is not enumerated, so looking before raising
    // reports "no splitlane window" for an app that is running perfectly well.
    //
    // But raise *every* splitlane, not one: with a dev build and the installed
    // app both running, raising only the first would leave the other's window
    // unlisted and the two-window guard below would pass on a coin toss - the
    // exact failure it exists to stop. Both up, then look, then raise the one
    // that owns the window we mean.
    var window = splitlaneWindow()
    if window == nil {
        let apps = splitlaneApps()
        if apps.isEmpty { fail("ABORT: splitlane is not running") }
        for app in apps {
            app.activate(options: [])
            Thread.sleep(forTimeInterval: 0.35)
        }
        Thread.sleep(forTimeInterval: 0.4)
        window = splitlaneWindow()
    }
    guard let window else {
        fail(
            """
            ABORT: the window server will not name a splitlane window. Check
            that the app is running and not minimised or on another Space;
            failing that, this binary may lack Screen Recording permission
            (granted per executable path in System Settings > Privacy &
            Security > Screen Recording).
            """)
    }
    guard let app = NSRunningApplication(processIdentifier: window.pid) else {
        fail("ABORT: no running application owns window \(window.id)")
    }
    // Activation is a request, not a command: macOS refuses to move focus often
    // enough that a single attempt fails several times an hour. Ask up to three
    // times, then give up loudly - never proceed unfocused, because a keystroke
    // addressed to Splitlane lands in whatever IS frontmost, and that cost a
    // session once already.
    //
    // Two ways to ask, because the first one often will not work from here.
    // `NSRunningApplication.activate` is refused when the caller is itself a
    // background process launched from another app's terminal - measured: three
    // attempts in a row left Cursor frontmost. System Events *does* move focus,
    // because the terminal already holds Accessibility permission, so that is
    // the fallback rather than a nicety.
    // By pid, not by name: "a splitlane is frontmost" is the check that let
    // input land in the other one.
    var front: pid_t = 0
    var frontName = "?"
    for attempt in 0..<3 {
        if attempt == 0 {
            app.activate(options: [])
        } else {
            raiseViaSystemEvents(pid: window.pid)
        }
        Thread.sleep(forTimeInterval: attempt == 0 ? 1.0 : 0.9)
        let frontmost = NSWorkspace.shared.frontmostApplication
        front = frontmost?.processIdentifier ?? 0
        frontName = frontmost?.localizedName ?? "?"
        if front == window.pid { break }
    }
    guard front == window.pid else {
        fail("ABORT: frontmost is \(frontName) (pid \(front)) after 3 attempts - refusing to send input")
    }
    return window.rect
}

let source = CGEventSource(stateID: .hidSystemState)

func post(_ event: CGEvent?) {
    event?.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.05)
}

/// A key event carrying BOTH a virtual code and a character.
///
/// Both halves are load-bearing and each was found the hard way. GPUI reads the
/// character, so a bare virtual code for Return arrives as a newline in the
/// composer instead of a submit. The terminal wants the code as well, so a
/// character alone is not enough either.
func key(_ code: CGKeyCode, char: UniChar? = nil, flags: CGEventFlags = []) {
    for down in [true, false] {
        guard let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)
        else { continue }
        if var unichar = char {
            event.keyboardSetUnicodeString(stringLength: 1, unicodeString: &unichar)
        }
        event.flags = flags
        post(event)
    }
}

/// Literal text, one character at a time, through `keyboardSetUnicodeString`.
///
/// This is what makes scripted typing reliable. System Events' `keystroke` maps
/// through the active keyboard layout, so under a Russian layout every letter
/// arrived as the one on the physical `A` key ("permission" became "фффффффффф" - the observed output,
/// kept because it is the evidence) and the resulting
/// screenshot looked like a meaningful negative result. Unicode strings do not
/// consult the layout at all.
/// US-layout virtual key codes for the printable ASCII this helper types.
///
/// The unicode string below is what makes a *terminal* receive the right
/// character whatever the active layout is, and it is not enough on its own:
/// a GPUI text field reads the **key**, not the string, so every character
/// sent on `virtualKey: 0` arrived in it as `a` - typing "build-log" into the
/// slot header's rename produced "aaaaaaaaa", and the screenshot of that is a
/// convincing wrong answer. Measured 29 August 2026, and it is the same shape
/// as the keyboard-layout failure above: input that looks delivered and is not.
///
/// A shifted character sends its unshifted key with the shift flag, so the
/// field sees a capital and the terminal still sees the string.
///
/// This makes the ACTIVE KEYBOARD LAYOUT matter again for a text field, which
/// the unicode string alone had made irrelevant: the field asks the OS what
/// the key produces. Type ASCII with a Latin layout selected. A character not
/// in the table (Cyrillic, an emoji) still goes out as a unicode string on key
/// 0, which the terminal reads correctly and a text field does not.
let usKeyCodes: [Character: (CGKeyCode, Bool)] = {
    let unshifted = "asdfhgzxcv\u{0}bqweryt123465=97-80]ou[ip\u{0}lj'k;\\,/nm."
    var table: [Character: (CGKeyCode, Bool)] = [:]
    for (index, character) in unshifted.enumerated() where character != "\u{0}" {
        table[character] = (CGKeyCode(index), false)
    }
    table["`"] = (50, false)
    table[" "] = (49, false)
    // The shifted half, by the key that carries it on a US layout.
    let shifted: [Character: Character] = [
        "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8",
        "(": "9", ")": "0", "_": "-", "+": "=", "{": "[", "}": "]", "|": "\\", ":": ";",
        "\"": "'", "<": ",", ">": ".", "?": "/", "~": "`",
    ]
    for (upper, base) in shifted {
        if let (code, _) = table[base] { table[upper] = (code, true) }
    }
    for letter in "abcdefghijklmnopqrstuvwxyz" {
        if let (code, _) = table[letter] {
            table[Character(letter.uppercased())] = (code, true)
        }
    }
    return table
}()

func typeText(_ text: String) {
    for character in text {
        var utf16 = Array(String(character).utf16)
        // The key the character sits on, when we know it. An unknown character
        // (Cyrillic, an emoji) still goes through as a unicode string on key
        // 0, which is what the terminal reads - it is only a text field that
        // would misread it, and this helper types ASCII into those.
        let (code, shifted) = usKeyCodes[character] ?? (0, false)
        for down in [true, false] {
            guard let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)
            else { continue }
            // Always assigned, never left at the default: an unset `flags`
            // inherits the live hardware modifier state, so one shifted
            // character latched shift for the rest of the string ("Build-Log
            // 42!" arrived as "BUILD_LOG $@!").
            event.flags = shifted ? .maskShift : []
            event.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: &utf16)
            event.post(tap: .cghidEventTap)
        }
        Thread.sleep(forTimeInterval: 0.012)
    }
}

let args = Array(CommandLine.arguments.dropFirst())
guard let command = args.first else {
    fail("usage: eyecheck window | click <x> <y> | type <text> | key <code> [unichar] | enter | esc | shifttab")
}

switch command {
case "window":
    // Raises first, like every other subcommand: an off-screen window is not
    // enumerated, and this is the call most likely to be made cold.
    _ = focusSplitlane()
    guard let window = splitlaneWindow() else { fail("ABORT: no splitlane window") }
    print(
        "\(window.id) \(Int(window.rect.origin.x)) \(Int(window.rect.origin.y)) "
            + "\(Int(window.rect.width)) \(Int(window.rect.height))")

case "click":
    guard args.count >= 3, let x = Double(args[1]), let y = Double(args[2]) else {
        fail("usage: eyecheck click <x-in-window> <y-in-window>")
    }
    let rect = focusSplitlane()
    let point = CGPoint(x: rect.origin.x + x, y: rect.origin.y + y)
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: point,
            mouseButton: .left))
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: point,
            mouseButton: .left))
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: point,
            mouseButton: .left))
    print("clicked \(Int(point.x)),\(Int(point.y))")

case "dclick":
    // A real double click, not two clicks in a row: macOS carries the count in
    // the event itself (`mouseEventClickState`), and a widget that listens for
    // a double click - the slot header's inline rename is the only way into
    // that gesture - never sees one otherwise.
    guard args.count >= 3, let x = Double(args[1]), let y = Double(args[2]) else {
        fail("usage: eyecheck dclick <x-in-window> <y-in-window>")
    }
    let drect = focusSplitlane()
    let dpoint = CGPoint(x: drect.origin.x + x, y: drect.origin.y + y)
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: dpoint,
            mouseButton: .left))
    for state in [1, 2] {
        for type in [CGEventType.leftMouseDown, CGEventType.leftMouseUp] {
            if let event = CGEvent(
                mouseEventSource: source, mouseType: type, mouseCursorPosition: dpoint,
                mouseButton: .left)
            {
                event.setIntegerValueField(.mouseEventClickState, value: Int64(state))
                post(event)
            }
        }
    }
    print("double-clicked \(Int(dpoint.x)),\(Int(dpoint.y))")

case "rclick":
    guard args.count >= 3, let x = Double(args[1]), let y = Double(args[2]) else {
        fail("usage: eyecheck rclick <x-in-window> <y-in-window>")
    }
    let rrect = focusSplitlane()
    let rpoint = CGPoint(x: rrect.origin.x + x, y: rrect.origin.y + y)
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: rpoint,
            mouseButton: .left))
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .rightMouseDown, mouseCursorPosition: rpoint,
            mouseButton: .right))
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .rightMouseUp, mouseCursorPosition: rpoint,
            mouseButton: .right))
    print("right-clicked \(Int(rpoint.x)),\(Int(rpoint.y))")

case "drag":
    // Press at one point, move there in steps, release at the other. The steps
    // are the point: GPUI starts a drag on movement while the button is down,
    // and a single jump from press to release is a click that happened to land
    // somewhere else.
    guard args.count >= 5, let x1 = Double(args[1]), let y1 = Double(args[2]),
        let x2 = Double(args[3]), let y2 = Double(args[4])
    else {
        fail("usage: eyecheck drag <from-x> <from-y> <to-x> <to-y>")
    }
    let grect = focusSplitlane()
    let from = CGPoint(x: grect.origin.x + x1, y: grect.origin.y + y1)
    let to = CGPoint(x: grect.origin.x + x2, y: grect.origin.y + y2)
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: from,
            mouseButton: .left))
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: from,
            mouseButton: .left))
    let steps = 12
    for step in 1...steps {
        let t = Double(step) / Double(steps)
        let point = CGPoint(
            x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t)
        post(
            CGEvent(
                mouseEventSource: source, mouseType: .leftMouseDragged,
                mouseCursorPosition: point, mouseButton: .left))
    }
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: to,
            mouseButton: .left))
    print("dragged \(Int(from.x)),\(Int(from.y)) -> \(Int(to.x)),\(Int(to.y))")

case "hover":
    guard args.count >= 3, let x = Double(args[1]), let y = Double(args[2]) else {
        fail("usage: eyecheck hover <x-in-window> <y-in-window>")
    }
    let hrect = focusSplitlane()
    let hpoint = CGPoint(x: hrect.origin.x + x, y: hrect.origin.y + y)
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: hpoint,
            mouseButton: .left))
    print("hovering \(Int(hpoint.x)),\(Int(hpoint.y))")

case "scroll":
    guard args.count >= 4, let x = Double(args[1]), let y = Double(args[2]),
        let amount = Int32(args[3])
    else {
        fail("usage: eyecheck scroll <x-in-window> <y-in-window> <lines: negative scrolls down>")
    }
    let srect = focusSplitlane()
    let spoint = CGPoint(x: srect.origin.x + x, y: srect.origin.y + y)
    post(
        CGEvent(
            mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: spoint,
            mouseButton: .left))
    if let ev = CGEvent(
        scrollWheelEvent2Source: source, units: .line, wheelCount: 1, wheel1: amount, wheel2: 0,
        wheel3: 0)
    {
        ev.location = spoint
        post(ev)
    }
    print("scrolled \(amount) at \(Int(spoint.x)),\(Int(spoint.y))")

case "type":
    guard args.count >= 2 else { fail("usage: eyecheck type <text>") }
    _ = focusSplitlane()
    typeText(args.dropFirst().joined(separator: " "))
    print("typed")

case "key":
    guard args.count >= 2, let code = UInt16(args[1]) else {
        fail("usage: eyecheck key <virtual-code> [unichar]")
    }
    _ = focusSplitlane()
    key(code, char: args.count >= 3 ? UInt16(args[2]) : nil)
    print("key \(code)")

case "enter":
    _ = focusSplitlane()
    key(36, char: 13)  // Return, plus CR - see `key`
    print("enter")

case "esc":
    _ = focusSplitlane()
    key(53, char: 27)
    print("esc")

case "shifttab":
    _ = focusSplitlane()
    key(48, flags: .maskShift)
    print("shift-tab")

case "chord":
    // The app's own bindings are mostly chords, so a helper that can only send
    // bare keys cannot reach most of what it needs to look at.
    guard args.count >= 3, let code = UInt16(args[2]) else {
        fail("usage: eyecheck chord <cmd,shift,alt,ctrl> <virtual-code>")
    }
    var flags: CGEventFlags = []
    for name in args[1].split(separator: ",") {
        switch name.lowercased() {
        case "cmd", "command": flags.insert(.maskCommand)
        case "shift": flags.insert(.maskShift)
        case "alt", "option": flags.insert(.maskAlternate)
        case "ctrl", "control": flags.insert(.maskControl)
        default: fail("unknown modifier: \(name)")
        }
    }
    _ = focusSplitlane()
    key(code, flags: flags)
    print("chord \(args[1]) \(code)")

default:
    fail("unknown command: \(command)")
}
