/* SPDX-License-Identifier: GPL-3.0-or-later
 * Android entry point for mirai. GTK's Android backend dlopen()s libmirai.so and calls
 * main(); the program itself is Rust (crates/mirai), linked in as a static library.
 *
 * Android discards stdout/stderr, so both are forwarded to logcat (tag "mirai") — that is
 * where tracing-subscriber and GLib's warnings end up. For debugging, environment variables
 * can be injected with `adb shell setprop debug.mirai.env "RUST_LOG=debug GSK_DEBUG=renderer"`. */
#include <android/log.h>
#include <glib.h>
#include <pthread.h>
#include <stdio.h>
#include <sys/system_properties.h>
#include <unistd.h>

extern int mirai_android_main (int argc, char **argv);

/* Private GLib API, exported by pixiewood's glib "hack" patch; the GTK runtime uses it too. */
extern void g_set_user_dirs (const char *first_dir_type, ...) G_GNUC_NULL_TERMINATED;

static int log_pipe[2];

static void *
log_thread (void *data)
{
  char buf[1024];
  FILE *f = fdopen (log_pipe[0], "r");

  while (f && fgets (buf, sizeof buf, f))
    __android_log_write (ANDROID_LOG_INFO, "mirai", buf);
  return NULL;
}

static void
android_setup (void)
{
  char val[PROP_VALUE_MAX] = { 0 };
  pthread_t thread;

  /* No terminal: plain tracing output, no ANSI colours. */
  g_setenv ("NO_COLOR", "1", FALSE);

  /* The GTK runtime points GLib's user dirs at the app's (external) files dir with
   * g_set_user_dirs(), which sets no environment variables, while mirai finds its config
   * and data through $XDG_* / $HOME (the `directories` crate). Export what GLib knows, and
   * give everything a cache that is the app's own: Context.getCacheDir(), i.e. the
   * `cache` dir beside the internal `files` dir whose `etc` the runtime made
   * XDG_CONFIG_DIRS. Without it GLib falls back to $HOME (/data) and fontconfig finds
   * "No writable cache directories" and rescans every font on every start. */
  {
    const char *const *sys_config = g_get_system_config_dirs ();
    const char *const *sys_data = g_get_system_data_dirs ();
    char *data_dirs = g_strjoinv (":", (char **) sys_data);
    char *files = g_path_get_dirname (sys_config[0]);   /* …/files/etc -> …/files */
    char *appdir = g_path_get_dirname (files);           /* /data/user/0/<package> */
    char *cache = g_build_filename (appdir, "cache", NULL);
    char *fonts_conf = g_build_filename (sys_config[0], "fonts", "fonts.conf", NULL);

    g_mkdir_with_parents (cache, 0700);
    g_set_user_dirs ("HOME", files, "XDG_CACHE_HOME", cache, NULL);

    g_setenv ("XDG_CONFIG_HOME", g_get_user_config_dir (), FALSE);
    g_setenv ("XDG_DATA_HOME", g_get_user_data_dir (), FALSE);
    g_setenv ("XDG_DATA_DIRS", data_dirs, FALSE);
    g_setenv ("XDG_CACHE_HOME", cache, TRUE);
    g_setenv ("HOME", files, TRUE);
    /* fontconfig's built-in path (/etc/fonts) does not exist; use the fonts.conf pixiewood
     * ships in the assets, whose <cachedir prefix="xdg"> is $XDG_CACHE_HOME/fontconfig. */
    if (g_file_test (fonts_conf, G_FILE_TEST_IS_REGULAR))
      {
        char *fonts_dir = g_path_get_dirname (fonts_conf);

        g_setenv ("FONTCONFIG_FILE", fonts_conf, FALSE);
        g_setenv ("FONTCONFIG_PATH", fonts_dir, FALSE);
        g_free (fonts_dir);
      }
    g_free (data_dirs);
    g_free (files);
    g_free (appdir);
    g_free (cache);
    g_free (fonts_conf);
  }

  if (__system_property_get ("debug.mirai.env", val) > 0)
    {
      char **pairs = g_strsplit (val, " ", -1);

      for (char **p = pairs; *p; p++)
        {
          char **kv = g_strsplit (*p, "=", 2);

          if (kv[0] && kv[1] && *kv[0])
            g_setenv (kv[0], kv[1], TRUE);
          g_strfreev (kv);
        }
      g_strfreev (pairs);
    }

  if (pipe (log_pipe) == 0)
    {
      dup2 (log_pipe[1], STDOUT_FILENO);
      dup2 (log_pipe[1], STDERR_FILENO);
      setvbuf (stdout, NULL, _IOLBF, 0);
      setvbuf (stderr, NULL, _IOLBF, 0);
      if (pthread_create (&thread, NULL, log_thread, NULL) == 0)
        pthread_detach (thread);
    }
}

int
main (int argc, char **argv)
{
  android_setup ();
  return mirai_android_main (argc, argv);
}
