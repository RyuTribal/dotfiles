import QtQuick
import QtTest
import qs.modules.common
import ShellUnderTest

// ConfigErrorToast's text, with the stand-in Config of fakes/qs/modules/common. Run it through
// backend_tests.sh, which builds the ShellUnderTest module.
TestCase {
    id: tc
    name: "ConfigErrorToast"
    when: windowShown

    Component {
        id: toastComponent
        ConfigErrorToast {}
    }

    function toastFor(message) {
        Config.parseError = message;
        return createTemporaryObject(toastComponent, tc);
    }

    function test_textNamesTheMessageWithoutALine() {
        const toast = toastFor("JSON.parse: Parse error");
        compare(toast.text, "config.json has an error; changes are not saved until it is fixed: JSON.parse: Parse error");
    }
}
