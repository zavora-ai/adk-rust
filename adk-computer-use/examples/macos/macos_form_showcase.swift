import AppKit

final class ShowcaseDelegate: NSObject, NSApplicationDelegate {
    private var window: NSWindow!
    private var nameField: NSTextField!
    private var projectField: NSTextField!
    private var statusLabel: NSTextField!

    func applicationDidFinishLaunching(_ notification: Notification) {
        let frame = NSRect(x: 0, y: 0, width: 520, height: 330)
        window = NSWindow(
            contentRect: frame,
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "ADK Form Showcase"
        window.center()

        let content = NSView(frame: frame)
        window.contentView = content

        let heading = NSTextField(labelWithString: "Governed multi-agent form completion")
        heading.frame = NSRect(x: 36, y: 260, width: 450, height: 30)
        heading.font = .boldSystemFont(ofSize: 19)
        content.addSubview(heading)

        let explanation = NSTextField(wrappingLabelWithString:
            "ADK observes this window, requests approval in PiP, and only then fills these public demonstration fields through the safety runtime.")
        explanation.frame = NSRect(x: 36, y: 205, width: 448, height: 48)
        explanation.textColor = .secondaryLabelColor
        content.addSubview(explanation)

        let nameLabel = NSTextField(labelWithString: "Name")
        nameLabel.frame = NSRect(x: 36, y: 164, width: 100, height: 24)
        content.addSubview(nameLabel)
        nameField = NSTextField(frame: NSRect(x: 150, y: 160, width: 330, height: 28))
        nameField.placeholderString = "Public demo name"
        nameField.setAccessibilityLabel("Name")
        content.addSubview(nameField)

        let projectLabel = NSTextField(labelWithString: "Project")
        projectLabel.frame = NSRect(x: 36, y: 116, width: 100, height: 24)
        content.addSubview(projectLabel)
        projectField = NSTextField(frame: NSRect(x: 150, y: 112, width: 330, height: 28))
        projectField.placeholderString = "Public demo project"
        projectField.setAccessibilityLabel("Project")
        content.addSubview(projectField)

        let submit = NSButton(title: "Verify form", target: self, action: #selector(verifyForm))
        submit.frame = NSRect(x: 350, y: 58, width: 130, height: 34)
        submit.bezelStyle = .rounded
        submit.setAccessibilityLabel("Verify form")
        content.addSubview(submit)

        statusLabel = NSTextField(labelWithString: "Waiting for governed execution")
        statusLabel.frame = NSRect(x: 36, y: 64, width: 300, height: 24)
        statusLabel.setAccessibilityLabel("Form status")
        content.addSubview(statusLabel)

        window.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
    }

    @objc private func verifyForm() {
        statusLabel.stringValue = nameField.stringValue.isEmpty || projectField.stringValue.isEmpty
            ? "Form is incomplete"
            : "Form values are present"
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

let app = NSApplication.shared
let delegate = ShowcaseDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
