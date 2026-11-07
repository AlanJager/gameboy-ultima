# GBA Test ROMs

`arm.gba`, `thumb.gba`, and `memory.gba` are test ROMs from
[jsmolka/gba-tests](https://github.com/jsmolka/gba-tests), licensed under the
MIT License. They are intended for emulator testing and are not commercial game
ROMs.

Run the integration tests with:

```sh
cargo test --test arm_gba --test thumb_gba --test memory_gba
```

These ROMs validate CPU and memory behavior; they do not establish full
commercial-game compatibility or complete GBA hardware support.
