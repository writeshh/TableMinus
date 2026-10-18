// Vite's `?raw` imports: a file's text, bundled into the app (the changelog for Help › Release Notes).
declare module "*?raw" {
  const text: string;
  export default text;
}
