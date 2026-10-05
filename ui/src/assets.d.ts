// Vite turns an imported asset into its URL.
declare module "*.png" {
  const url: string;
  export default url;
}
