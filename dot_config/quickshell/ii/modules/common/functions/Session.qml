pragma Singleton
import Quickshell
import qs.services
import qs.modules.common

Singleton {
    id: root

    // The session actions run in axon (Power.qml); poweroff, reboot and logout close the windows first.
    function lock() {
        Power.lock();
    }

    function suspend() {
        Power.suspend();
    }

    function logout() {
        Power.logout();
    }

    function launchTaskManager() {
        Quickshell.execDetached(["bash", "-c", `${Config.options.apps.taskManager}`]);
    }

    function hibernate() {
        Power.hibernate();
    }

    function poweroff() {
        Power.poweroff();
    }

    function reboot() {
        Power.reboot();
    }

    function rebootToFirmware() {
        Power.rebootToFirmware();
    }
}
