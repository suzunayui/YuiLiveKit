/* SPDX-License-Identifier: GPL-2.0-or-later
 *
 * Adds the YuiLiveKit overlay parts to OBS as browser sources, so the user needs neither
 * OBS WebSocket nor a password. Each part is its own source, placed straight in the scene
 * and not in a group, so each can go in front of or behind any other source (comments
 * behind a 3D model, flying comments in front of it) by dragging it in OBS's list.
 *
 * Nothing is added by itself: not every stream uses every part, so the Tools menu has one
 * item per part, and each adds only that part to the current scene.
 *
 * When the user has asked for it in the app, the plugin also starts the app, without its
 * window, once OBS has loaded, and tells it to go away when OBS closes. Everything the app
 * does stays in the app's own process, so a fault there cannot take OBS down.
 *
 * It also reloads those sources when the app starts. A browser source that loaded while
 * the app was not running shows an error page, and that page cannot reconnect by itself.
 */
#include <obs-module.h>
#include <obs-frontend-api.h>
#include <graphics/vec2.h>
#include <util/platform.h>
#include <util/dstr.h>
#include <sys/stat.h>
#include <string.h>
#include <windows.h>

OBS_DECLARE_MODULE()

/* Written by the app in its data folder: %APPDATA%\<Tauri identifier>. */
#define SOURCE_FILE "com.suzunayui.yuilivekit/obs-source.json"
/* Also written by the app: where it is, and whether OBS should start it. */
#define PLUGIN_FILE "com.suzunayui.yuilivekit/obs-plugin.json"
#define LOG_PREFIX "[yui-live-kit] "

struct part {
	const char *name;
	const char *url;
	long long width, height, x, y;
};

/* Used when the app has not written its file yet. Back to front, as the app writes them. */
static const struct part default_parts[] = {
	{"YuiLiveKit - 参加管理", "http://127.0.0.1:5000/overlay/participants", 500, 600, 1380, 40},
	{"YuiLiveKit - おはよう", "http://127.0.0.1:5000/overlay/ohayo", 380, 120, 1500, 920},
	{"YuiLiveKit - コメント", "http://127.0.0.1:5000/overlay/comments", 720, 636, 24, 420},
	{"YuiLiveKit - 字幕", "http://127.0.0.1:5000/overlay/subtitles", 1600, 250, 160, 800},
	{"YuiLiveKit - ニコニコ風", "http://127.0.0.1:5000/overlay/niconico", 1920, 880, 0, 100},
};
#define DEFAULT_COUNT (sizeof(default_parts) / sizeof(default_parts[0]))
#define MAX_PARTS 16

/* The app rewrites its file each time its server starts; a new time means it just started. */
static time_t file_time;
static float since_check;
#define CHECK_SECONDS 2.0f
#define MAX_REFRESH 64

MODULE_EXPORT const char *obs_module_description(void)
{
	return "Adds the YuiLiveKit overlay parts as browser sources";
}

/* Reads the parts from the app's file; `file` keeps the strings alive until released. */
static size_t read_parts(struct part *parts, obs_data_t **file, double *canvas_width)
{
	*canvas_width = 1920.0;
	char *path = os_get_config_path_ptr(SOURCE_FILE);
	*file = path ? obs_data_create_from_json_file(path) : NULL;
	size_t count = 0;
	if (*file) {
		if (obs_data_get_double(*file, "canvas_width") > 0)
			*canvas_width = obs_data_get_double(*file, "canvas_width");

		obs_data_array_t *sources = obs_data_get_array(*file, "sources");
		size_t total = sources ? obs_data_array_count(sources) : 0;
		for (size_t i = 0; i < total && count < MAX_PARTS; i++) {
			/* The array keeps a reference, so the strings outlive this release. */
			obs_data_t *item = obs_data_array_item(sources, i);
			struct part part = {obs_data_get_string(item, "name"), obs_data_get_string(item, "url"),
					    obs_data_get_int(item, "width"),       obs_data_get_int(item, "height"),
					    obs_data_get_int(item, "x"),           obs_data_get_int(item, "y")};
			if (*part.name && *part.url && part.width > 0 && part.height > 0)
				parts[count++] = part;
			obs_data_release(item);
		}
		obs_data_array_release(sources);
	}
	if (!count) {
		blog(LOG_INFO, LOG_PREFIX "no app settings at %s, using the defaults", path ? path : "(none)");
		for (size_t i = 0; i < DEFAULT_COUNT; i++)
			parts[i] = default_parts[i];
		count = DEFAULT_COUNT;
		*canvas_width = 1920.0;
	}
	bfree(path);
	return count;
}

static obs_source_t *get_or_create(const struct part *part)
{
	obs_source_t *source = obs_get_source_by_name(part->name);
	if (source)
		return source;
	obs_data_t *settings = obs_data_create();
	obs_data_set_string(settings, "url", part->url);
	obs_data_set_int(settings, "width", part->width);
	obs_data_set_int(settings, "height", part->height);
	source = obs_source_create("browser_source", part->name, settings, NULL);
	obs_data_release(settings);
	return source;
}

/* Adds one part to the current scene, at its usual place and on top of what is there. A part
 * the scene already has is left alone. */
static void add_part(const char *name)
{
	obs_source_t *scene_source = obs_frontend_get_current_scene();
	obs_scene_t *scene = scene_source ? obs_scene_from_source(scene_source) : NULL;
	if (!scene) {
		obs_source_release(scene_source);
		blog(LOG_WARNING, LOG_PREFIX "no current scene");
		return;
	}

	struct part parts[MAX_PARTS];
	obs_data_t *file = NULL;
	double canvas_width;
	size_t count = read_parts(parts, &file, &canvas_width);
	const struct part *part = NULL;
	for (size_t i = 0; i < count; i++) {
		if (strcmp(parts[i].name, name) == 0)
			part = &parts[i];
	}

	if (!part) {
		blog(LOG_WARNING, LOG_PREFIX "the app does not list '%s'", name);
	} else if (obs_scene_find_source_recursive(scene, part->name)) {
		blog(LOG_INFO, LOG_PREFIX "'%s' is already in scene '%s'", part->name, obs_source_get_name(scene_source));
	} else {
		obs_source_t *source = get_or_create(part);
		obs_sceneitem_t *item = source ? obs_scene_add(scene, source) : NULL;
		if (item) {
			/* Positions are for the app's canvas; scale them to the one OBS uses. */
			float scale = 1.0f;
			struct obs_video_info ovi;
			if (obs_get_video_info(&ovi) && canvas_width > 0)
				scale = (float)(ovi.base_width / canvas_width);
			struct vec2 pos, size;
			vec2_set(&pos, (float)part->x * scale, (float)part->y * scale);
			vec2_set(&size, scale, scale);
			obs_sceneitem_set_pos(item, &pos);
			obs_sceneitem_set_scale(item, &size);
			blog(LOG_INFO, LOG_PREFIX "added '%s' to scene '%s'", part->name, obs_source_get_name(scene_source));
		} else {
			blog(LOG_WARNING, LOG_PREFIX "browser_source is not available in this OBS");
		}
		obs_source_release(source);
	}

	obs_data_release(file);
	obs_source_release(scene_source);
}

static time_t app_file_time(void)
{
	char *path = os_get_config_path_ptr(SOURCE_FILE);
	struct stat st;
	time_t time = path && os_stat(path, &st) == 0 ? st.st_mtime : 0;
	bfree(path);
	return time;
}

struct overlay_sources {
	char base[256];
	obs_source_t *found[MAX_REFRESH];
	size_t count;
};

/* Collects the browser sources that show the app's pages, whoever added them. */
static bool collect_overlay_source(void *param, obs_source_t *source)
{
	struct overlay_sources *list = param;
	if (list->count >= MAX_REFRESH || strcmp(obs_source_get_unversioned_id(source), "browser_source") != 0)
		return true;
	obs_data_t *settings = obs_source_get_settings(source);
	const char *url = obs_data_get_string(settings, "url");
	if (!obs_data_get_bool(settings, "is_local_file") && strncmp(url, list->base, strlen(list->base)) == 0) {
		obs_source_t *kept = obs_source_get_ref(source);
		if (kept)
			list->found[list->count++] = kept;
	}
	obs_data_release(settings);
	return true;
}

/* Reloads every browser source whose URL is one of the app's pages. Runs on the UI thread. */
static void refresh_overlay_sources(void *data)
{
	UNUSED_PARAMETER(data);
	struct overlay_sources list = {.base = "http://127.0.0.1:5000/overlay/"};
	struct part parts[MAX_PARTS];
	obs_data_t *file = NULL;
	double canvas_width;
	if (read_parts(parts, &file, &canvas_width)) {
		const char *marker = strstr(parts[0].url, "/overlay/");
		size_t length = marker ? (size_t)(marker - parts[0].url) + strlen("/overlay/") : 0;
		if (length && length < sizeof(list.base)) {
			memcpy(list.base, parts[0].url, length);
			list.base[length] = '\0';
		}
	}
	obs_data_release(file);

	obs_enum_sources(collect_overlay_source, &list);
	for (size_t i = 0; i < list.count; i++) {
		obs_properties_t *props = obs_source_properties(list.found[i]);
		obs_property_t *refresh = props ? obs_properties_get(props, "refreshnocache") : NULL;
		if (refresh)
			obs_property_button_clicked(refresh, list.found[i]);
		obs_properties_destroy(props);
		obs_source_release(list.found[i]);
	}
	blog(LOG_INFO, LOG_PREFIX "the app started; reloaded %d source(s) under %s", (int)list.count, list.base);
}

static void on_tick(void *data, float seconds)
{
	UNUSED_PARAMETER(data);
	since_check += seconds;
	if (since_check < CHECK_SECONDS)
		return;
	since_check = 0.0f;
	time_t time = app_file_time();
	if (time && time != file_time) {
		file_time = time;
		obs_queue_task(OBS_TASK_UI, refresh_overlay_sources, NULL, false);
	} else if (time) {
		file_time = time;
	}
}

/* Whether this OBS started the app, so that only then it is told to go away. */
static bool started_app;

/* Runs the app with one argument: "--background" to start it without its window (or, if it
 * is running, to keep it running), "--quit-background" to end it unless its window is open.
 * `only_if_wanted` checks the user's setting first. Returns whether it was run. */
static bool run_app(const char *argument, bool only_if_wanted)
{
	char *path = os_get_config_path_ptr(PLUGIN_FILE);
	obs_data_t *file = path ? obs_data_create_from_json_file(path) : NULL;
	bfree(path);
	if (!file)
		return false;

	bool ran = false;
	const char *app = obs_data_get_string(file, "app");
	if (only_if_wanted && !obs_data_get_bool(file, "start_with_obs")) {
		/* Off unless the user turned it on in the app. */
	} else if (!*app || !os_file_exists(app)) {
		blog(LOG_WARNING, LOG_PREFIX "the app is not at '%s'", app);
	} else {
		struct dstr command = {0};
		dstr_printf(&command, "\"%s\" %s", app, argument);
		wchar_t *wide = NULL;
		os_utf8_to_wcs_ptr(command.array, command.len, &wide);
		STARTUPINFOW startup = {0};
		startup.cb = sizeof(startup);
		PROCESS_INFORMATION process = {0};
		if (wide && CreateProcessW(NULL, wide, NULL, NULL, FALSE, 0, NULL, NULL, &startup, &process)) {
			CloseHandle(process.hThread);
			CloseHandle(process.hProcess);
			ran = true;
			blog(LOG_INFO, LOG_PREFIX "ran the app with %s", argument);
		} else {
			blog(LOG_WARNING, LOG_PREFIX "could not run the app (error %lu)", GetLastError());
		}
		bfree(wide);
		dstr_free(&command);
	}
	obs_data_release(file);
	return ran;
}

static void on_event(enum obs_frontend_event event, void *data)
{
	UNUSED_PARAMETER(data);
	if (event == OBS_FRONTEND_EVENT_FINISHED_LOADING) {
		started_app = run_app("--background", true);
	} else if (event == OBS_FRONTEND_EVENT_EXIT && started_app) {
		started_app = false;
		run_app("--quit-background", false);
	}
}

/* The Tools menu has one item per part; `data` is the part's name. */
static void on_menu(void *data)
{
	add_part(data);
}

static char menu_labels[DEFAULT_COUNT][160];

bool obs_module_load(void)
{
	file_time = app_file_time();
	obs_add_tick_callback(on_tick, NULL);
	obs_frontend_add_event_callback(on_event, NULL);
	for (size_t i = 0; i < DEFAULT_COUNT; i++) {
		snprintf(menu_labels[i], sizeof(menu_labels[i]), "%s を今のシーンに追加", default_parts[i].name);
		obs_frontend_add_tools_menu_item(menu_labels[i], on_menu, (void *)default_parts[i].name);
	}
	return true;
}

void obs_module_unload(void)
{
	obs_remove_tick_callback(on_tick, NULL);
	obs_frontend_remove_event_callback(on_event, NULL);
}
