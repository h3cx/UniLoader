# UniLoader

A simple daemon that automatically imports any libraries into KiCad. It currently supports symbols and footprints from [SamacSys](https://componentsearchengine.com/), [UltraLibrarian](https://app.ultralibrarian.com/search), [SnapMagic](https://www.snapeda.com/home/)

## Installation

An automated install script is planned but currently the repo needs to be cloned, built and installed manually.
```
git clone https://git.h3cx.dev/h3cx/UniLoader.git

cd UniLoader

cargo build -r
```
You will then find the binary under `target/release` you can run it manually or create a systemd service to automatically run it at boot.

## Usage

To get started you will need to set up your config, to do so create a config file. A full config file can be found in the config folder of this repo.
```
mkdir ~/.config/uniloader

touch ~/.config/uniloader/config.toml
```
Then use your favorite text editor to open this config and paste in the minimal config.
```
[libraries]
path = "~/KiCad/libraries/"
```
Now that you have set this up, the application should be ready to go, just head over to your favorite library website and download any component.

Upon downloading the first component from a given online library you will be required to add the library to KiCad. In KiCad open your symbol editor and head to `File->Add Library` then naviagate to the path you set in your config and add the library. Repeat this from the footprint editor for the footprints libraries.

## Warranty

Use this program at your own risk, no warranty is provided for anything that it could do to your libraries, KiCad or operating system. If you encounter any issues feel free to open an issue.
