wasm-pack build --target web
vercel --prod;vercel alias temty.vercel.app
cargo run --bin admin -- reset-db
railway up


wasm-pack build --target web;vercel --prod;vercel alias temty.vercel.app


#!/bin/sh
curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
wasm-pack build --target web

