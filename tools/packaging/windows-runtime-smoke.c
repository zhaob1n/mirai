// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
// Run from an installed bin/ with no MSYS2 DLL/data overrides. --play also
// exercises the real audio sink; CI prepares the stream without needing a device.
#include <gtk/gtk.h>
#include <gst/gst.h>
#include <libsoup/soup.h>
#include <string.h>

typedef struct {
    GSocketListener *listener;
    GCancellable *cancel;
    char *request;
    GError *error;
} Proxy;

static gpointer serve_proxy(gpointer data)
{
    Proxy *proxy = data;
    GSocketConnection *connection = g_socket_listener_accept(
        proxy->listener, NULL, proxy->cancel, &proxy->error);
    if (!connection)
        return NULL;
    g_socket_set_timeout(g_socket_connection_get_socket(connection), 5);
    GDataInputStream *input = g_data_input_stream_new(
        g_io_stream_get_input_stream(G_IO_STREAM(connection)));
    proxy->request = g_data_input_stream_read_line(input, NULL, proxy->cancel, &proxy->error);
    if (proxy->request) {
        char *line;
        while ((line = g_data_input_stream_read_line(input, NULL, proxy->cancel, &proxy->error))) {
            gboolean end = !*line || strcmp(line, "\r") == 0;
            g_free(line);
            if (end)
                break;
        }
    }
    if (!proxy->error) {
        static const char reply[] = "HTTP/1.1 200 OK\r\nContent-Length: 18\r\n"
            "Connection: close\r\n\r\nmirai proxy smoke\n";
        g_output_stream_write_all(g_io_stream_get_output_stream(G_IO_STREAM(connection)),
            reply, sizeof(reply) - 1, NULL, proxy->cancel, &proxy->error);
    }
    g_object_unref(input);
    g_io_stream_close(G_IO_STREAM(connection), NULL, NULL);
    g_object_unref(connection);
    return NULL;
}

static void check_proxy(void)
{
    Proxy proxy = { .listener = g_socket_listener_new(), .cancel = g_cancellable_new() };
    GSocketAddress *address = g_inet_socket_address_new_from_string("127.0.0.1", 0);
    GSocketAddress *bound = NULL;
    GError *error = NULL;
    g_socket_listener_add_address(proxy.listener, address, G_SOCKET_TYPE_STREAM,
        G_SOCKET_PROTOCOL_TCP, NULL, &bound, &error);
    g_assert_no_error(error);
    char *url = g_strdup_printf("http://127.0.0.1:%u",
        g_inet_socket_address_get_port(G_INET_SOCKET_ADDRESS(bound)));
    g_unsetenv("HTTP_PROXY");
    g_unsetenv("HTTPS_PROXY");
    g_unsetenv("NO_PROXY");
    g_unsetenv("GIO_USE_PROXY_RESOLVER");
    g_setenv("http_proxy", url, TRUE);
    g_setenv("https_proxy", url, TRUE);
    g_setenv("no_proxy", "", TRUE);
    char **proxies = g_proxy_resolver_lookup(g_proxy_resolver_get_default(),
        "https://mirai.invalid/kifu", NULL, &error);
    g_assert_no_error(error);
    g_assert_cmpstr(proxies[0], ==, url);
    g_strfreev(proxies);

    GThread *thread = g_thread_new("proxy", serve_proxy, &proxy);
    SoupSession *session = soup_session_new_with_options("timeout", 5, NULL);
    SoupMessage *message = soup_message_new("GET", "http://mirai.invalid/kifu");
    GBytes *body = soup_session_send_and_read(session, message, NULL, &error);
    g_cancellable_cancel(proxy.cancel);
    g_thread_join(thread);
    g_assert_no_error(error);
    g_assert_no_error(proxy.error);
    g_assert_cmpuint(soup_message_get_status(message), ==, SOUP_STATUS_OK);
    g_assert_true(g_str_has_prefix(proxy.request, "GET http://mirai.invalid/kifu "));
    gsize size;
    const char *bytes = g_bytes_get_data(body, &size);
    g_assert_cmpmem(bytes, size, "mirai proxy smoke\n", 18);
    g_print("runtime-smoke: HTTP request reached configured proxy; HTTPS proxy lookup passed\n");
    g_bytes_unref(body);
    g_object_unref(message);
    g_object_unref(session);
    g_free(proxy.request);
    g_free(url);
    g_object_unref(bound);
    g_object_unref(address);
    g_object_unref(proxy.listener);
    g_object_unref(proxy.cancel);
}

static void check_image(const char *format, const char *bytes, gsize size, int tolerance)
{
    GError *error = NULL;
    GInputStream *input = g_memory_input_stream_new_from_data(bytes, size, NULL);
    GdkPixbuf *image = gdk_pixbuf_new_from_stream(input, NULL, &error);
    g_assert_no_error(error);
    g_assert_cmpint(gdk_pixbuf_get_width(image), ==, 17);
    g_assert_cmpint(gdk_pixbuf_get_height(image), ==, 11);
    const guchar *pixel = gdk_pixbuf_read_pixels(image);
    const int expected[] = { 0x4a, 0x7f, 0xbe };
    for (int channel = 0; channel < 3; channel++)
        g_assert_cmpint(ABS((int)pixel[channel] - expected[channel]), <=, tolerance);
    g_print("runtime-smoke: %s decoded correct dimensions and pixel colour\n", format);
    g_object_unref(image);
    g_object_unref(input);
}

static void check_images(void)
{
    GdkPixbuf *source = gdk_pixbuf_new(GDK_COLORSPACE_RGB, FALSE, 8, 17, 11);
    gdk_pixbuf_fill(source, 0x4a7fbeff);
    const char *formats[] = { "png", "jpeg", "tiff" };
    for (guint i = 0; i < G_N_ELEMENTS(formats); i++) {
        GError *error = NULL;
        char *bytes = NULL;
        gsize size;
        gdk_pixbuf_save_to_buffer(source, &bytes, &size, formats[i], &error, NULL);
        g_assert_no_error(error);
        check_image(formats[i], bytes, size, strcmp(formats[i], "jpeg") == 0 ? 3 : 0);
        g_free(bytes);
    }
    g_object_unref(source);
    static const char svg[] = "<svg xmlns='http://www.w3.org/2000/svg' width='17' height='11'>"
        "<rect width='17' height='11' fill='#4a7fbe'/></svg>";
    check_image("svg fallback", svg, sizeof(svg) - 1, 0);
}

static void check_icons(void)
{
    static const char path[] = "/io/github/zhaob1n/Mirai/icons/hicolor/scalable/actions";
    GError *error = NULL;
    char **icons = g_resources_enumerate_children(path, G_RESOURCE_LOOKUP_FLAGS_NONE, &error);
    g_assert_no_error(error);
    GtkIconTheme *theme = gtk_icon_theme_get_for_display(gdk_display_get_default());
    gtk_icon_theme_add_resource_path(theme, "/io/github/zhaob1n/Mirai/icons/hicolor");
    for (guint i = 0; icons[i]; i++) {
        char *name = g_strndup(icons[i], strlen(icons[i]) - strlen(".svg"));
        GtkIconPaintable *icon = gtk_icon_theme_lookup_icon(theme, name, NULL, 24, 1,
            GTK_TEXT_DIR_LTR, 0);
        char *uri = g_file_get_uri(gtk_icon_paintable_get_file(icon));
        char *expected = g_strdup_printf("resource://%s/%s", path, icons[i]);
        g_assert_cmpstr(uri, ==, expected);
        g_print("runtime-smoke: %s resolved its embedded action SVG\n", name);
        g_free(expected);
        g_free(uri);
        g_object_unref(icon);
        g_free(name);
    }
    g_strfreev(icons);
}

static void check_audio_sink(void)
{
    gst_init(NULL, NULL);
    GstElement *sink = gst_element_factory_make("wasapi2sink", NULL);
    g_assert_nonnull(sink);
    GstPad *pad = gst_element_get_static_pad(sink, "sink");
    GstCaps *caps = gst_pad_query_caps(pad, NULL);
    GstCaps *pcm = gst_caps_from_string(
        "audio/x-raw,format=S16LE,layout=interleaved,rate=48000,channels=1");
    g_assert_true(gst_caps_can_intersect(caps, pcm));
    g_print("runtime-smoke: WASAPI2 accepts the stone clips' 48 kHz mono PCM\n");
    gst_caps_unref(pcm);
    gst_caps_unref(caps);
    gst_object_unref(pad);
    gst_object_unref(sink);
}

static void put_u32(guint8 *dest, guint32 value)
{
    value = GUINT32_TO_LE(value);
    memcpy(dest, &value, sizeof(value));
}

static void check_audio(gboolean play)
{
    // Half a second of non-silent mono, 48 kHz, 16-bit PCM, using the app's memory-WAV path.
    guint8 wav[44 + 48000] = { 0 };
    memcpy(wav, "RIFF", 4);
    put_u32(wav + 4, sizeof(wav) - 8);
    memcpy(wav + 8, "WAVEfmt ", 8);
    put_u32(wav + 16, 16);
    wav[20] = 1;
    wav[22] = 1;
    put_u32(wav + 24, 48000);
    put_u32(wav + 28, 96000);
    wav[32] = 2;
    wav[34] = 16;
    memcpy(wav + 36, "data", 4);
    put_u32(wav + 40, sizeof(wav) - 44);
    for (guint i = 0; i < 24000; i++) {
        int phase = i % 48;
        gint16 sample = (phase < 24 ? phase - 12 : 36 - phase) * 682;
        guint16 encoded = GUINT16_TO_LE((guint16)sample);
        memcpy(wav + 44 + i * 2, &encoded, sizeof(encoded));
    }
    GInputStream *input = g_memory_input_stream_new_from_data(wav, sizeof(wav), NULL);
    GtkMediaStream *stream = gtk_media_file_new_for_input_stream(input);
    gint64 deadline = g_get_monotonic_time() + 10 * G_TIME_SPAN_SECOND;
    while (!gtk_media_stream_is_prepared(stream) && !gtk_media_stream_get_error(stream)
        && g_get_monotonic_time() < deadline) {
        g_main_context_iteration(NULL, FALSE);
        g_usleep(1000);
    }
    g_assert_no_error(gtk_media_stream_get_error(stream));
    g_assert_true(gtk_media_stream_is_prepared(stream));
    g_assert_cmpint(gtk_media_stream_get_duration(stream), ==, 500000);
    if (play) {
        gtk_media_stream_play(stream);
        deadline = g_get_monotonic_time() + 10 * G_TIME_SPAN_SECOND;
        while (!gtk_media_stream_get_ended(stream) && !gtk_media_stream_get_error(stream)
            && g_get_monotonic_time() < deadline) {
            g_main_context_iteration(NULL, FALSE);
            g_usleep(1000);
        }
        g_assert_no_error(gtk_media_stream_get_error(stream));
        g_assert_true(gtk_media_stream_get_ended(stream));
    }
    g_print("runtime-smoke: memory WAV prepared with duration 500000 us%s\n", play ? "; playback reached EOS" : "");
    g_object_unref(stream);
    g_object_unref(input);
}

int main(int argc, char **argv)
{
    gboolean play = argc == 2 && strcmp(argv[1], "--play") == 0;
    if (argc != 1 && !play) {
        g_printerr("usage: windows-runtime-smoke.exe [--play]\n");
        return 1;
    }
    check_proxy();
    check_images();
    gtk_init();
    check_icons();
    check_audio_sink();
    check_audio(play);
    return 0;
}
