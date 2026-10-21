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
   * and data through $XDG_* / $HOME (the `directories` crate). Export what GLib knows. */
  {
    const char *const *sys_data = g_get_system_data_dirs ();
    char *data_dirs = g_strjoinv (":", (char **) sys_data);
    char *home = g_path_get_dirname (g_get_user_data_dir ());

    g_setenv ("XDG_CONFIG_HOME", g_get_user_config_dir (), FALSE);
    g_setenv ("XDG_DATA_HOME", g_get_user_data_dir (), FALSE);
    g_setenv ("XDG_DATA_DIRS", data_dirs, FALSE);
    g_setenv ("XDG_CACHE_HOME", g_get_user_cache_dir (), FALSE);
    g_setenv ("HOME", home, FALSE);
    g_free (data_dirs);
    g_free (home);
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
