# Android build of mirai (GTK 4 Android backend, packaged by pixiewood).
# The application is the Rust crate crates/mirai, built by cargo as a static library and
# linked into libmirai.so together with GTK, libadwaita and libsoup built from subprojects.
project('mirai-android', 'c',
  version: '0.1.0',
  meson_version: '>= 1.9.0',
  default_options: ['warning_level=1', 'c_std=gnu11'])

assert(host_machine.system() == 'android', 'this project only builds for Android (see README)')

cc = meson.get_compiler('c')

gtk_dep = dependency('gtk4', version: '>= 4.22')
adw_dep = dependency('libadwaita-1', version: '>= 1.9')
soup_dep = dependency('libsoup-3.0', version: '>= 3.0')
intl_dep = dependency('intl')
# Linked directly by the Rust code (rustc --print native-static-libs).
direct_deps = [
  dependency('gio-2.0'),
  dependency('pango'),
  dependency('cairo'),
  dependency('cairo-gobject'),
  dependency('gdk-pixbuf-2.0'),
  dependency('graphene-gobject-1.0'),
]
log_dep = cc.find_library('log')
unwind_dep = cc.find_library('unwind', required: false)
dl_dep = cc.find_library('dl')
m_dep = cc.find_library('m')

rust_target = {
  'aarch64': 'aarch64-linux-android',
  'x86_64': 'x86_64-linux-android',
  'arm': 'armv7-linux-androideabi',
  'x86': 'i686-linux-android',
}[host_machine.cpu_family()]

# cargo tracks its own inputs, so always run it and let it decide what is stale.
libmirai_rust = custom_target('libmirai_android.a',
  output: 'libmirai_android.a',
  command: [files('cargo-build.sh'), rust_target, cc.cmd_array()[0],
            meson.current_build_dir() / 'cargo', '@OUTPUT@'],
  env: {'MIRAI_HOSTTOOLS': get_option('hosttools')},
  build_always_stale: true,
  console: true)

executable('mirai', 'main.c',
  dependencies: [gtk_dep, adw_dep, soup_dep, intl_dep, direct_deps, log_dep, unwind_dep, dl_dep, m_dep],
  link_with: libmirai_rust,
  android_exe_type: 'application',
  install: true)

# pixiewood reads the AppStream metainfo (app id, name, version) and wants it in the
# AppStream namespace, which data/…metainfo.xml.in leaves out.
custom_target('metainfo',
  input: '../data/io.github.zhaob1n.Mirai.metainfo.xml.in',
  output: 'io.github.zhaob1n.Mirai.metainfo.xml',
  command: [find_program('sed'),
            's|<component type=|<component xmlns="https://specifications.freedesktop.org/metainfo/1.0" type=|',
            '@INPUT@'],
  capture: true,
  build_by_default: true)
