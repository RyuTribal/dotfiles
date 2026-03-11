.pragma library

function build(parserPath, configPath) {
    return ["python3", "-E", parserPath, "--path", configPath];
}
