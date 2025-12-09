wasm-pack build --target web
vercel --prod;vercel alias temty.vercel.app
cargo run --bin admin -- reset-db
railway up