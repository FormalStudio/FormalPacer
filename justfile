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

# The app, the layer and its manifest, the desktop entry and the icon, for this user.
install:
    cargo build --release -p formalpacer -p formalpacer-layer
    install -Dm755 target/release/formalpacer ~/.local/bin/formalpacer
    install -Dm755 target/release/libformalpacer_layer.so ~/.local/lib/formalpacer/libformalpacer_layer.so
    mkdir -p ~/.local/share/vulkan/implicit_layer.d
    sed "s|@LIBRARY_PATH@|$HOME/.local/lib/formalpacer/libformalpacer_layer.so|" packaging/linux/formalpacer-layer.json.in > ~/.local/share/vulkan/implicit_layer.d/formalpacer.json
    install -Dm644 packaging/linux/dev.formalsnake.formalpacer.desktop ~/.local/share/applications/dev.formalsnake.formalpacer.desktop
    install -Dm644 packaging/linux/dev.formalsnake.formalpacer.png ~/.local/share/icons/hicolor/512x512/apps/dev.formalsnake.formalpacer.png

# Start in the tray at login.
autostart:
    install -Dm644 packaging/linux/dev.formalsnake.formalpacer-autostart.desktop ~/.config/autostart/dev.formalsnake.formalpacer.desktop

uninstall:
    rm -f ~/.local/bin/formalpacer ~/.local/lib/formalpacer/libformalpacer_layer.so ~/.local/share/vulkan/implicit_layer.d/formalpacer.json ~/.local/share/applications/dev.formalsnake.formalpacer.desktop ~/.local/share/icons/hicolor/512x512/apps/dev.formalsnake.formalpacer.png ~/.config/autostart/dev.formalsnake.formalpacer.desktop
    rmdir --ignore-fail-on-non-empty ~/.local/lib/formalpacer

# fakegame at each rate under the headless engine, with the layer from this checkout.
pacing-check *rates="200 100 60":
    scripts/pacing-check.sh {{rates}}
