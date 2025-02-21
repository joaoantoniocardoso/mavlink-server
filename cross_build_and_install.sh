#!/usr/bin/env bash

set -e

TARGET=armv7-unknown-linux-gnueabihf
BUILDTYPE=release

cross build --$BUILDTYPE --target=$TARGET

/home/joao/BlueRobotics/cross_build_dev/old/upload_to_blueos.sh \
    target/$TARGET/$BUILDTYPE/mavlink-server \
    /home/pi/mavlink-server

echo ""
echo ""
echo 'sshpass -p raspberry scp -o StrictHostKeyChecking=no pi@localhost:/home/pi/mavlink-server $(which mavp2p)'
echo ""
echo ""
