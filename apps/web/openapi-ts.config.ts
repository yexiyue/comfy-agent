import { defineConfig } from '@hey-api/openapi-ts'

export default defineConfig({
  input: '../../docs/api/openapi.json',
  output: { path: 'src/api/generated', importFileExtension: '.js' },
  plugins: [
    '@hey-api/typescript',
    '@hey-api/client-fetch',
    '@hey-api/sdk',
    '@tanstack/react-query',
  ],
})
