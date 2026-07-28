@Entry
@Component
struct RouterHome {
  openDetail() {
    router.pushUrl({ url: "pages/RouterDetail" })
  }

  build() {
    Text("home")
  }
}
