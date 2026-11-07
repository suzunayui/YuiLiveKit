/* SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Adds the YuiLiveKit overlay to OBS as one browser source, so the user needs
 * neither OBS WebSocket nor a password.
 */
#include <obs-module.h>
#include <obs-frontend-api.h>
#include <graphics/vec2.h>
#include <util/platform.h>

OBS_DECLARE_MODULE()

#define SOURCE_NAME "YuiLiveKit"
#define DEFAULT_URL "http://127.0.0.1:5000/overlay"
/* Written by the app in its data folder: %APPDATA%\<Tauri identifier>. */
#define SOURCE_FILE "com.suzunayui.yuilivekit/obs-source.json"
#define LOG_PREFIX "[yui-live-kit] "

static bool loaded;

MODULE_EXPORT const char *obs_module_description(void)
{
	return "Adds the YuiLiveKit overlay as a browser source";
}

static obs_data_t *source_settings(void)
{
	obs_data_t *settings = obs_data_create();
	obs_data_set_string(settings, "url", DEFAULT_URL);
	obs_data_set_int(settings, "width", 1920);
	obs_data_set_int(settings, "height", 1080);

	char *path = os_get_config_path_ptr(SOURCE_FILE);
	obs_data_t *file = path ? obs_data_create_from_json_file(path) : NULL;
	if (file) {
		const char *url = obs_data_get_string(file, "url");
		if (*url)
			obs_data_set_string(settings, "url", url);
		if (obs_data_get_int(file, "width") > 0)
			obs_data_set_int(settings, "width", obs_data_get_int(file, "width"));
		if (obs_data_get_int(file, "height") > 0)
			obs_data_set_int(settings, "height", obs_data_get_int(file, "height"));
		obs_data_release(file);
	} else {
		blog(LOG_INFO, LOG_PREFIX "no app settings at %s, using %s", path ? path : "(none)", DEFAULT_URL);
	}
	bfree(path);
	return settings;
}

/* Fits the item to the canvas, whatever resolution OBS uses. */
static void fit_to_canvas(obs_sceneitem_t *item)
{
	struct obs_video_info ovi;
	if (!obs_get_video_info(&ovi))
		return;
	struct vec2 bounds;
	vec2_set(&bounds, (float)ovi.base_width, (float)ovi.base_height);
	obs_sceneitem_set_bounds_type(item, OBS_BOUNDS_SCALE_INNER);
	obs_sceneitem_set_bounds(item, &bounds);
}

/* Puts the source in the current scene, creating it first if it does not exist yet. */
static bool add_to_current_scene(void)
{
	obs_source_t *scene_source = obs_frontend_get_current_scene();
	obs_scene_t *scene = scene_source ? obs_scene_from_source(scene_source) : NULL;
	if (!scene) {
		obs_source_release(scene_source);
		blog(LOG_WARNING, LOG_PREFIX "no current scene");
		return false;
	}

	obs_source_t *source = obs_get_source_by_name(SOURCE_NAME);
	bool created = false;
	if (!source) {
		obs_data_t *settings = source_settings();
		source = obs_source_create("browser_source", SOURCE_NAME, settings, NULL);
		obs_data_release(settings);
		created = source != NULL;
	}
	if (!source) {
		obs_source_release(scene_source);
		blog(LOG_WARNING, LOG_PREFIX "browser_source is not available in this OBS");
		return false;
	}

	if (!obs_scene_find_source(scene, SOURCE_NAME)) {
		obs_sceneitem_t *item = obs_scene_add(scene, source);
		if (item)
			fit_to_canvas(item);
		blog(LOG_INFO, LOG_PREFIX "added %s source to scene '%s'", created ? "a new" : "the existing",
		     obs_source_get_name(scene_source));
	}
	obs_source_release(source);
	obs_source_release(scene_source);
	return true;
}

/* Adds the source once per scene collection, so a source the user removed stays removed. */
static void add_once_for_collection(void)
{
	char *collection = obs_frontend_get_current_scene_collection();
	char *path = obs_module_config_path("added.json");
	obs_data_t *added = path ? obs_data_create_from_json_file_safe(path, "bak") : NULL;
	if (!added)
		added = obs_data_create();

	if (collection && path && !obs_data_get_bool(added, collection)) {
		if (add_to_current_scene()) {
			obs_data_set_bool(added, collection, true);
			char *dir = obs_module_config_path("");
			if (dir)
				os_mkdirs(dir);
			bfree(dir);
			if (!obs_data_save_json_safe(added, path, "tmp", "bak"))
				blog(LOG_WARNING, LOG_PREFIX "could not save %s", path);
		}
	} else if (collection) {
		blog(LOG_INFO, LOG_PREFIX "already added once to collection '%s'", collection);
	}

	obs_data_release(added);
	bfree(path);
	bfree(collection);
}

static void on_event(enum obs_frontend_event event, void *data)
{
	UNUSED_PARAMETER(data);
	if (event == OBS_FRONTEND_EVENT_FINISHED_LOADING) {
		loaded = true;
		add_once_for_collection();
	} else if (event == OBS_FRONTEND_EVENT_SCENE_COLLECTION_CHANGED && loaded) {
		add_once_for_collection();
	}
}

static void on_menu(void *data)
{
	UNUSED_PARAMETER(data);
	add_to_current_scene();
}

bool obs_module_load(void)
{
	obs_frontend_add_event_callback(on_event, NULL);
	obs_frontend_add_tools_menu_item("YuiLiveKit を今のシーンに追加", on_menu, NULL);
	return true;
}

void obs_module_unload(void)
{
	obs_frontend_remove_event_callback(on_event, NULL);
}
