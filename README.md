A CLI-based tool for Linux to prevent keyboard chatter by intercepting keyboard events at the OS level.

## How to use

1. Find the input device node that represents your keyboard using `evtest`
2. Build the binary with `cargo build --release`
3. Run the program with `sudo ./target/release/keyboard-debouncer /dev/input/eventX`, change `eventX` as the event node you've discovered

## Config

Currently, the list of keys to debounce are hardcoded in the program. You have to modify it at the source code and compile it yourself

## License

This app is licensed under GPLv3
