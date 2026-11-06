// Moves data from the app's former folders the first time YuiLiveKit starts.
const fs = require("node:fs");
const path = require("node:path");

const USER_DATA_NAME = "YuiLiveKit";
const LEGACY_USER_DATA_NAMES = ["yt-overlay", "YouTubeOverlay"];
const DATABASE_FILE = "comments.db";

function databaseModifiedAt(dir) {
  try {
    return fs.statSync(path.join(dir, DATABASE_FILE)).mtimeMs;
  } catch {
    return null;
  }
}

function newestLegacyDir(appData, legacyNames) {
  let newest = null;
  for (const name of legacyNames) {
    const dir = path.join(appData, name);
    const modifiedAt = databaseModifiedAt(dir);
    if (modifiedAt !== null && (!newest || modifiedAt > newest.modifiedAt)) {
      newest = { dir, modifiedAt };
    }
  }
  return newest?.dir ?? null;
}

/**
 * Renames the most recently used legacy folder to `target`. Every launch creates
 * comments.db, so a target without one has never held the user's data. A rename
 * fails on Windows while the old app has the folder open, and the error is
 * thrown so the caller can ask the user to close it.
 */
function migrateUserData({ appData, target, legacyNames = LEGACY_USER_DATA_NAMES, now = Date.now }) {
  if (databaseModifiedAt(target) !== null) return { status: "current" };
  const source = newestLegacyDir(appData, legacyNames);
  if (!source) return { status: "none" };

  let setAside = null;
  if (fs.existsSync(target)) {
    setAside = `${target}.before-migration-${now()}`;
    fs.renameSync(target, setAside);
  }
  try {
    fs.renameSync(source, target);
  } catch (error) {
    if (setAside) fs.renameSync(setAside, target);
    throw error;
  }
  return { status: "migrated", source, setAside };
}

module.exports = { USER_DATA_NAME, LEGACY_USER_DATA_NAMES, migrateUserData };
