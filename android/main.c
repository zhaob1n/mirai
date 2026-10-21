/* SPDX-License-Identifier: GPL-3.0-or-later
 * Android entry point for mirai. GTK's Android backend dlopen()s libmirai.so and calls
 * main(); the program itself is Rust (crates/mirai), linked in as a static library.
 *
 * Android discards stdout/stderr, so both are forwarded to logcat (tag "mirai") — that is
 * where tracing-subscriber and GLib's warnings end up. For debugging, environment variables
 * can be injected with `adb shell setprop debug.mirai.env "RUST_LOG=debug GSK_DEBUG=renderer"`. */
#include <android/log.h>
#include <gio/gio.h>
#include <glib.h>
#include <glib/gstdio.h>
#include <libsoup/soup.h>
#include <openssl/err.h>
#include <openssl/ssl.h>
#include <openssl/x509v3.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <sys/socket.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/system_properties.h>
#include <unistd.h>

extern int mirai_android_main (int argc, char **argv);

/* glib-networking's OpenSSL backend, linked in as a static GIO module. */
extern void g_io_openssl_load (GIOModule *module);

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


/* OpenSSL looks up a CA directory by the new subject hash, while Android names the files in
 * its CA store by the old one, so the store cannot be used as SSL_CERT_DIR. Concatenate it
 * into one PEM file in the cache instead (each file is PEM plus a text dump, which OpenSSL
 * skips) and refresh it when the store changes. Android 14 moved the store into the
 * Conscrypt APEX. */
static void
setup_ca_bundle (const char *cache)
{
  static const char *const stores[] = { "/apex/com.android.conscrypt/cacerts",
                                        "/system/etc/security/cacerts" };
  char *bundle = g_build_filename (cache, "cacerts.pem", NULL);

  for (gsize i = 0; i < G_N_ELEMENTS (stores); i++)
    {
      GStatBuf dir_st, bundle_st;
      GDir *dir;
      GString *pem;
      const char *name;

      if (g_stat (stores[i], &dir_st) != 0 || !(dir = g_dir_open (stores[i], 0, NULL)))
        continue;
      if (g_stat (bundle, &bundle_st) == 0 && bundle_st.st_mtime >= dir_st.st_mtime)
        {
          g_dir_close (dir);
          break;
        }
      pem = g_string_new (NULL);
      while ((name = g_dir_read_name (dir)))
        {
          char *path = g_build_filename (stores[i], name, NULL);
          char *contents;
          gsize len;

          if (g_file_get_contents (path, &contents, &len, NULL))
            {
              g_string_append_len (pem, contents, len);
              g_string_append_c (pem, '\n');
              g_free (contents);
            }
          g_free (path);
        }
      g_dir_close (dir);
      if (!g_file_set_contents (bundle, pem->str, pem->len, NULL))
        __android_log_print (ANDROID_LOG_WARN, "mirai", "cannot write %s", bundle);
      g_string_free (pem, TRUE);
      break;
    }
  g_setenv ("SSL_CERT_FILE", bundle, FALSE);
  g_free (bundle);
}

/* Debug aid: `setprop debug.mirai.env "MIRAI_HTTPS_TEST=https://example.com/"` fetches the
 * URL once with libsoup at startup and logs the outcome, including certificate checks. */
static gpointer
https_test_thread (gpointer url)
{
  SoupSession *session = soup_session_new ();
  SoupMessage *msg = soup_message_new ("GET", url);
  GError *error = NULL;
  GBytes *body;

  if (!msg)
    {
      __android_log_print (ANDROID_LOG_ERROR, "mirai-https", "bad URL %s", (char *) url);
      return NULL;
    }
  body = soup_session_send_and_read (session, msg, NULL, &error);
  if (body)
    {
      GTlsCertificate *cert = soup_message_get_tls_peer_certificate (msg);
      char *issuer = cert ? g_tls_certificate_get_issuer_name (cert) : NULL;

      __android_log_print (ANDROID_LOG_INFO, "mirai-https",
                           "GET %s -> HTTP %u, %zu bytes, TLS %s, peer cert errors 0x%x, issuer %s",
                           (char *) url, soup_message_get_status (msg), g_bytes_get_size (body),
                           cert ? "yes" : "no",
                           (unsigned) soup_message_get_tls_peer_certificate_errors (msg),
                           issuer ? issuer : "-");
      g_free (issuer);
      g_bytes_unref (body);
    }
  else
    {
      __android_log_print (ANDROID_LOG_ERROR, "mirai-https", "GET %s failed: %s (%s %d)",
                           (char *) url, error->message, g_quark_to_string (error->domain),
                           error->code);
      g_error_free (error);
    }
  g_object_unref (msg);
  g_object_unref (session);
  return NULL;
}

static int
log_openssl_error (const char *str, size_t len, void *u)
{
  __android_log_print (ANDROID_LOG_ERROR, "mirai-https", "openssl: %.*s", (int) len, str);
  return 1;
}

/* The same handshake straight through OpenSSL, to see its whole error queue. */
static void
raw_openssl_test (const char *host, int port, const char *name)
{
  struct sockaddr_in sa = { .sin_family = AF_INET, .sin_port = htons (port) };
  SSL_CTX *ctx = SSL_CTX_new (TLS_client_method ());
  SSL *ssl;
  int fd = socket (AF_INET, SOCK_STREAM, 0), ret;

  inet_pton (AF_INET, host, &sa.sin_addr);
  if (connect (fd, (struct sockaddr *) &sa, sizeof sa) != 0)
    {
      __android_log_print (ANDROID_LOG_ERROR, "mirai-https", "raw: connect failed");
      close (fd);
      SSL_CTX_free (ctx);
      return;
    }
  if (!g_getenv ("SSL_CERT_FILE") ||
      !SSL_CTX_load_verify_locations (ctx, g_getenv ("SSL_CERT_FILE"), NULL))
    SSL_CTX_set_default_verify_paths (ctx);
  SSL_CTX_set_verify (ctx, SSL_VERIFY_PEER, NULL);
  ssl = SSL_new (ctx);
  SSL_set_fd (ssl, fd);
  SSL_set_tlsext_host_name (ssl, name);
  SSL_set1_host (ssl, name);
  ret = SSL_connect (ssl);
  __android_log_print (ANDROID_LOG_INFO, "mirai-https",
                       "raw OpenSSL handshake: ret %d, SSL_get_error %d, verify %ld (%s), %s",
                       ret, SSL_get_error (ssl, ret), SSL_get_verify_result (ssl),
                       X509_verify_cert_error_string (SSL_get_verify_result (ssl)),
                       SSL_get_version (ssl));
  ERR_print_errors_cb (log_openssl_error, NULL);
  SSL_free (ssl);
  close (fd);
  SSL_CTX_free (ctx);
}

/* Debug aid for when the device has no network of its own:
 * `adb reverse tcp:8443 tcp:8443`, forward host port 8443 to www.example.com:443, then
 * `setprop debug.mirai.env "MIRAI_TLS_TEST=127.0.0.1:8443:www.example.com"`. This connects to
 * host:port, handshakes with SNI and certificate checks for the given name, and sends one
 * HTTP request, so the TLS backend and the CA bundle are exercised end to end. */
static gpointer
tls_test_thread (gpointer spec)
{
  char **parts = g_strsplit (spec, ":", 3);
  GSocketClient *client = g_socket_client_new ();
  GSocketConnection *conn = NULL;
  GSocketConnectable *identity = NULL;
  GIOStream *tls = NULL;
  GError *error = NULL;

  if (g_strv_length (parts) != 3)
    goto out;
  conn = g_socket_client_connect_to_host (client, parts[0], atoi (parts[1]), NULL, &error);
  if (!conn)
    goto out;
  identity = g_network_address_new (parts[2], 443);
  tls = g_tls_client_connection_new (G_IO_STREAM (conn), identity, &error);
  if (!tls || !g_tls_connection_handshake (G_TLS_CONNECTION (tls), NULL, &error))
    goto out;
  {
    GTlsCertificate *cert = g_tls_connection_get_peer_certificate (G_TLS_CONNECTION (tls));
    char *issuer = cert ? g_tls_certificate_get_issuer_name (cert) : NULL;
    char *request = g_strdup_printf ("GET / HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n",
                                     parts[2]);
    char reply[64] = { 0 };
    gssize n;

    g_output_stream_write_all (g_io_stream_get_output_stream (tls), request, strlen (request),
                               NULL, NULL, &error);
    n = g_input_stream_read (g_io_stream_get_input_stream (tls), reply, sizeof reply - 1, NULL,
                             NULL);
    if (n > 0)
      reply[strcspn (reply, "\r\n")] = 0;
    __android_log_print (ANDROID_LOG_INFO, "mirai-https",
                         "TLS to %s:%s as %s: handshake OK (%s), protocol %d, peer cert errors 0x%x, "
                         "issuer %s, reply \"%s\"",
                         parts[0], parts[1], parts[2],
                         G_OBJECT_TYPE_NAME (tls),
                         (int) g_tls_connection_get_protocol_version (G_TLS_CONNECTION (tls)),
                         (unsigned) g_tls_connection_get_peer_certificate_errors (G_TLS_CONNECTION (tls)),
                         issuer ? issuer : "-", reply);
    g_free (issuer);
    g_free (request);
  }
out:
  if (error)
    {
      __android_log_print (ANDROID_LOG_ERROR, "mirai-https", "TLS test %s failed: %s (%s %d)",
                           (char *) spec, error->message, g_quark_to_string (error->domain),
                           error->code);
      ERR_print_errors_cb (log_openssl_error, NULL);
      g_error_free (error);
      if (g_strv_length (parts) == 3)
        raw_openssl_test (parts[0], atoi (parts[1]), parts[2]);
    }
  g_clear_object (&tls);
  g_clear_object (&identity);
  g_clear_object (&conn);
  g_object_unref (client);
  g_strfreev (parts);
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
    setup_ca_bundle (cache);
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

  /* GIO finds TLS backends by scanning GIO_MODULE_DIR, which an APK does not have. */
  g_io_openssl_load (NULL);
  /* glib-networking's default database is X509_STORE_set_default_paths(), and OpenSSL reads
   * $SSL_CERT_FILE only through ossl_safe_getenv(), which ignores the environment when
   * getauxval(AT_SECURE) is set, as it is in app processes forked from the zygote. Make the
   * bundle the backend's default database instead (libsoup and everything else use it). */
  if (g_getenv ("SSL_CERT_FILE"))
    {
      GError *error = NULL;
      GTlsDatabase *db = g_tls_file_database_new (g_getenv ("SSL_CERT_FILE"), &error);

      if (db)
        {
          g_tls_backend_set_default_database (g_tls_backend_get_default (), db);
          g_object_unref (db);
        }
      else
        {
          __android_log_print (ANDROID_LOG_WARN, "mirai", "CA bundle: %s", error->message);
          g_error_free (error);
        }
    }
  if (g_getenv ("MIRAI_HTTPS_TEST"))
    g_thread_unref (g_thread_new ("https-test", https_test_thread,
                                  g_strdup (g_getenv ("MIRAI_HTTPS_TEST"))));
  if (g_getenv ("MIRAI_TLS_TEST"))
    g_thread_unref (g_thread_new ("tls-test", tls_test_thread,
                                  g_strdup (g_getenv ("MIRAI_TLS_TEST"))));

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
