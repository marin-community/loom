import { test, expect } from '../fixtures/weaver';

test('the SPA shell revalidates while content-hashed resources remain immutable', async ({ weaver }) => {
  const shell = await fetch(`${weaver.baseUrl}/`);
  expect(shell.status).toBe(200);
  expect(shell.headers.get('cache-control')).toBe('no-store, max-age=0');

  const lastModified = shell.headers.get('last-modified');
  expect(lastModified).toBeTruthy();
  const revalidatedShell = await fetch(`${weaver.baseUrl}/`, {
    headers: { 'If-Modified-Since': lastModified! },
  });
  expect(revalidatedShell.status).toBe(304);
  expect(revalidatedShell.headers.get('cache-control')).toBe('no-store, max-age=0');

  const html = await shell.text();
  const asset = html.match(/(?:src|href)="([^"]+\.[0-9a-fA-F]{8}\.(?:js|css))"/)?.[1];
  expect(asset, 'built index.html should reference a content-hashed JS or CSS asset').toBeTruthy();

  const resource = await fetch(new URL(asset!, weaver.baseUrl));
  expect(resource.status).toBe(200);
  expect(resource.headers.get('cache-control')).toBe('max-age=31536000, immutable');
});
