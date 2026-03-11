import QtQuick 2.15
import QtTest 1.2
import "../../services/hyprlandKeybindsCommand.js" as KeybindsCommand

TestCase {
    name: "HyprlandKeybindsCommand"

    function test_buildUsesPythonInterpreter() {
        const command = KeybindsCommand.build("/tmp/get_keybinds.py", "/tmp/keybinds.conf");

        compare(command.length, 5);
        compare(command[0], "python3");
        compare(command[1], "-E");
        compare(command[2], "/tmp/get_keybinds.py");
        compare(command[3], "--path");
        compare(command[4], "/tmp/keybinds.conf");
    }
}
