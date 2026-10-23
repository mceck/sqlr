

$(shell mkdir -p dist)

macos:
	scripts/package-macos.sh

install: macos
	cp -R dist/sqlr.app /Applications/