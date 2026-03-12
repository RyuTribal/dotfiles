.pragma library

function normalize(value) {
    return typeof value === "string" ? value.trim().toLowerCase() : "";
}

function normalizeSoundSpec(sound) {
    return typeof sound === "string" ? sound.trim() : "";
}

function isSuppressed(hints) {
    if (!hints || typeof hints !== "object")
        return false;

    return hints["suppress-sound"] === true || hints["suppress_sound"] === true;
}

function matchesOverride(notification, override) {
    if (!override || typeof override !== "object")
        return false;

    const expectedDesktopEntry = normalize(override.desktopEntry);
    const expectedAppName = normalize(override.appName);

    if (!expectedDesktopEntry && !expectedAppName)
        return false;

    if (expectedDesktopEntry && expectedDesktopEntry !== normalize(notification && notification.desktopEntry))
        return false;

    if (expectedAppName && expectedAppName !== normalize(notification && notification.appName))
        return false;

    return true;
}

function soundFromSpec(sound, source) {
    const spec = normalizeSoundSpec(sound);
    if (!spec)
        return null;

    if (spec.startsWith("file://"))
        return { kind: "file", value: spec.slice("file://".length), source: source };

    if (spec.startsWith("/"))
        return { kind: "file", value: spec, source: source };

    return { kind: "theme", value: spec, source: source };
}

function findMatchingOverride(notification, overrides) {
    if (!Array.isArray(overrides))
        return null;

    for (const override of overrides) {
        if (matchesOverride(notification, override))
            return override;
    }

    return null;
}

function resolve(notification, soundConfig) {
    const config = soundConfig || {};
    const hints = notification && notification.hints ? notification.hints : {};

    if (config.enabled === false || isSuppressed(hints))
        return null;

    const override = findMatchingOverride(notification, config.overrides);
    if (override)
        return soundFromSpec(override.sound, "override");

    const hintedFile = normalizeSoundSpec(hints["sound-file"] ?? hints["sound_file"] ?? "");
    if (hintedFile)
        return soundFromSpec(hintedFile, "hint-file");

    const hintedName = normalizeSoundSpec(hints["sound-name"] ?? hints["sound_name"] ?? "");
    if (hintedName)
        return soundFromSpec(hintedName, "hint-name");

    return soundFromSpec(config.defaultSound, "default");
}

function buildCommand(sound) {
    if (!sound || !sound.value)
        return [];

    if (sound.kind === "file")
        return ["canberra-gtk-play", "-f", sound.value];

    return ["canberra-gtk-play", "-i", sound.value];
}
