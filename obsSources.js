// Carries OBS browser sources created under the app's former name over to the new one.
const SOURCE_KINDS = [
  "Subtitles",
  "Comments",
  "Niconico",
  "Participants Vertical",
  "Participants Horizontal",
  "Ohayo",
];
const RENAMES = new Map(
  SOURCE_KINDS.map((kind) => [`YT Overlay - ${kind}`, `YuiLiveKit - ${kind}`])
);

function migrateSourceList(sources) {
  return String(sources || "")
    .split(",")
    .map((name) => name.trim())
    .filter(Boolean)
    .map((name) => RENAMES.get(name) || name)
    .join(", ");
}

/** Renames legacy inputs in place, so their position and filters in OBS survive. */
async function renameLegacyInputs(obs) {
  const { inputs = [] } = await obs.call("GetInputList", { inputKind: "browser_source" });
  const names = new Set(inputs.map((input) => input.inputName));
  for (const [legacyName, inputName] of RENAMES) {
    if (!names.has(legacyName) || names.has(inputName)) continue;
    try {
      await obs.call("SetInputName", { inputName: legacyName, newInputName: inputName });
      names.delete(legacyName);
      names.add(inputName);
    } catch (error) {
      console.warn("OBS source rename error:", legacyName, error?.message || error);
    }
  }
  return names;
}

/** Encrypts the password for saving; stays in plain text only where the OS offers no encryption. */
function sealPassword(settings, safeStorage) {
  if (!settings.password || !safeStorage.isEncryptionAvailable()) return settings;
  const { password, ...rest } = settings;
  return { ...rest, passwordEncrypted: safeStorage.encryptString(password).toString("base64") };
}

function openPassword(raw, safeStorage) {
  if (!raw.passwordEncrypted) return String(raw.password || "");
  try {
    return safeStorage.decryptString(Buffer.from(raw.passwordEncrypted, "base64"));
  } catch (error) {
    console.warn("OBS password could not be decrypted:", error?.message || error);
    return "";
  }
}

module.exports = { migrateSourceList, renameLegacyInputs, sealPassword, openPassword };
