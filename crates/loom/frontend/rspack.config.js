const path = require('path');
const { HtmlRspackPlugin } = require('@rspack/core');
const { VueLoaderPlugin } = require('rspack-vue-loader');

// One config, two modes. `npm run build` (rspack build --mode production) emits
// the hashed, on-disk bundle the loom server serves from `static/dist`.
// `npm run dev` (rspack serve --mode development) runs an in-memory dev server
// with Vue HMR, proxying every `/api` call — REST, the SSE event stream, and
// the terminal WebSocket — to the already-running loom backend. The dev server
// never writes to disk, so it leaves the production `static/dist` untouched.
module.exports = (_env, argv) => {
  const isDev = (argv && argv.mode) === 'development';

  // The running loom backend the dev server proxies to. Matches loom's default
  // bind address; override with WEAVER_API to point at a server elsewhere.
  const backend = process.env.WEAVER_API || 'http://127.0.0.1:7878';

  return {
    entry: './src/main.ts',
    output: {
      path: path.resolve(__dirname, '../static/dist'),
      filename: isDev ? 'app.js' : 'app.[contenthash:8].js',
      clean: !isDev,
    },
    resolve: {
      extensions: ['.ts', '.js', '.vue'],
    },
    plugins: [
      new VueLoaderPlugin(),
      new HtmlRspackPlugin({ template: './src/index.html', filename: 'index.html' }),
    ],
    module: {
      rules: [
        { test: /\.vue$/, loader: 'rspack-vue-loader', options: { experimentalInlineMatchResource: true } },
        { test: /\.ts$/, loader: 'builtin:swc-loader', options: { jsc: { parser: { syntax: 'typescript' } } }, type: 'javascript/auto' },
        { test: /\.css$/, use: ['postcss-loader'], type: 'css' },
      ],
    },
    experiments: {
      css: true,
    },
    mode: isDev ? 'development' : 'production',
    devtool: isDev ? 'eval-cheap-module-source-map' : false,
    devServer: {
      host: '127.0.0.1',
      port: 5178,
      hot: true,
      // Don't gzip; it buffers the `/api/.../events` SSE stream.
      compress: false,
      // SPA deep links (the app uses hash routing, but keep this for safety).
      historyApiFallback: true,
      client: {
        overlay: { errors: true, warnings: false },
      },
      proxy: [
        {
          context: ['/api'],
          target: backend,
          changeOrigin: true,
          // Upgrade /api/sessions/{id}/terminal to a proxied WebSocket.
          ws: true,
          // The backend's CSWSH guard (terminal.rs `origin_ok`) only accepts a
          // WebSocket whose Origin is loopback on its OWN bound port. The
          // browser's Origin here is the dev server (:5178) and `changeOrigin`
          // rewrites only Host — so rewrite Origin to the backend too, or the
          // terminal handshake 403s. Harmless for the REST routes (permissive CORS).
          headers: { origin: backend },
        },
      ],
    },
  };
};
