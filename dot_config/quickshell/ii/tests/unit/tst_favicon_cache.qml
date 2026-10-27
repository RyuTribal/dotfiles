import QtQuick
import QtTest
import Quickshell.Io
import ShellUnderTest

// The real services/FaviconCache.qml over a stand-in Process: one curl per host for the whole shell,
// and a host whose download failed is not tried again in the session. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "FaviconCache"

    SignalSpy {
        id: downloaded
        target: FaviconCache
        signalName: "downloaded"
    }

    function init() {
        ProcessProbe.started = [];
        downloaded.clear();
    }

    function test_manyRowsOfOneHostStartOneDownload() {
        FaviconCache.request("one.example");
        FaviconCache.request("one.example");
        FaviconCache.request("one.example");
        compare(ProcessProbe.started.length, 1);
        const argv = ProcessProbe.started[0].command;
        compare(argv[0], "curl");
        verify(argv.includes("/nonexistent/favicons/one.example.ico"), argv);
        verify(argv.includes("https://www.google.com/s2/favicons?domain=one.example&sz=32"), argv);
        verify(argv.includes("test-agent"), argv);
    }

    // Every waiting row is told once, and then the file is there: a later row loads it without a request.
    function test_aDownloadTellsEveryRowOnceAndIsNotRepeated() {
        FaviconCache.request("two.example");
        ProcessProbe.started[0].exited(0, 0);
        compare(downloaded.count, 1);
        compare(downloaded.signalArguments[0][0], "two.example");
        FaviconCache.request("two.example"); // a file that does not decode asks again
        compare(ProcessProbe.started.length, 1);
    }

    function test_aHostWithoutAFaviconIsNotTriedAgain() {
        FaviconCache.request("three.example");
        ProcessProbe.started[0].exited(22, 0); // curl --fail on a 404
        compare(downloaded.count, 0);
        FaviconCache.request("three.example");
        compare(ProcessProbe.started.length, 1);
    }

    function test_hostsAreIndependent() {
        FaviconCache.request("four.example");
        FaviconCache.request("five.example");
        compare(ProcessProbe.started.length, 2);
    }

    // A host with any other character (a quote, `$(`, a port, user info) runs nothing.
    function test_anInvalidHostStartsNothing() {
        for (const host of ["", "a b", "x.example:8080", "user@x.example", "$(rm -rf ~)", "x'.example"]) {
            verify(!FaviconCache.isValidHost(host), host);
            FaviconCache.request(host);
        }
        compare(ProcessProbe.started.length, 0);
    }
}
