build:
	cargo build
build_release:
	cargo build --release
aarch64:
	if [ -d "bin" ] ; then rm -rf bin/; fi
	mkdir bin
	cross build --release --target aarch64-unknown-linux-gnu
	cp target/aarch64-unknown-linux-gnu/release/alfred-audio-out bin/
