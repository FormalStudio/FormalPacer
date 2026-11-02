dev:
    cargo run

check:
    nd check

test:
    nd test

test-real:
    nd test --tier real

diagnose:
    nd diagnose --last

# The app, the layer and its manifest, the desktop entry, the icon and the login item, for this user.
install:
    cargo build --release -p formalpacer -p formalpacer-layer -p formalpacer-gl
    cargo build --release -p formalpacer-layer -p formalpacer-gl --target i686-unknown-linux-gnu
    install -Dm755 target/release/formalpacer ~/.local/bin/formalpacer
    install -Dm755 -t ~/.local/lib/formalpacer target/release/libformalpacer_layer.so target/release/libformalpacer_gl.so
    install -Dm755 -t ~/.local/lib/formalpacer/i686 target/i686-unknown-linux-gnu/release/libformalpacer_layer.so target/i686-unknown-linux-gnu/release/libformalpacer_gl.so
    mkdir -p ~/.local/share/vulkan/implicit_layer.d
    sed "s|@LAYER_NAME@|VK_LAYER_FORMALSTUDIO_formalpacer|; s|@LIBRARY_PATH@|$HOME/.local/lib/formalpacer/libformalpacer_layer.so|; s|@LIBRARY_ARCH@|64|" packaging/linux/formalpacer-layer.json.in > ~/.local/share/vulkan/implicit_layer.d/formalpacer.json
    sed "s|@LAYER_NAME@|VK_LAYER_FORMALSTUDIO_formalpacer_32|; s|@LIBRARY_PATH@|$HOME/.local/lib/formalpacer/i686/libformalpacer_layer.so|; s|@LIBRARY_ARCH@|32|" packaging/linux/formalpacer-layer.json.in > ~/.local/share/vulkan/implicit_layer.d/formalpacer-32.json
    install -Dm644 packaging/linux/dev.formalsnake.formalpacer.desktop ~/.local/share/applications/dev.formalsnake.formalpacer.desktop
    install -Dm644 packaging/linux/dev.formalsnake.formalpacer.png ~/.local/share/icons/hicolor/512x512/apps/dev.formalsnake.formalpacer.png
    mkdir -p ~/.config/autostart
    sed "s|^Exec=formalpacer|Exec=$HOME/.local/bin/formalpacer|" packaging/linux/dev.formalsnake.formalpacer-autostart.desktop > ~/.config/autostart/dev.formalsnake.formalpacer.desktop

uninstall:
    rm -f ~/.local/bin/formalpacer ~/.local/lib/formalpacer/libformalpacer_layer.so ~/.local/lib/formalpacer/libformalpacer_gl.so ~/.local/lib/formalpacer/i686/libformalpacer_layer.so ~/.local/lib/formalpacer/i686/libformalpacer_gl.so ~/.local/share/vulkan/implicit_layer.d/formalpacer.json ~/.local/share/vulkan/implicit_layer.d/formalpacer-32.json ~/.local/share/applications/dev.formalsnake.formalpacer.desktop ~/.local/share/icons/hicolor/512x512/apps/dev.formalsnake.formalpacer.png ~/.config/autostart/dev.formalsnake.formalpacer.desktop
    rmdir --ignore-fail-on-non-empty ~/.local/lib/formalpacer/i686 ~/.local/lib/formalpacer

# fakegame at each rate under the headless engine, with the layer from this checkout.
pacing-check *rates="200 100 60":
    scripts/pacing-check.sh {{rates}}
