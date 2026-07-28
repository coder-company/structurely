@Extend(Text)
function highlighted() {
  .fontWeight(FontWeight.Bold)
}

@Component
struct StyleHome {
  build() {
    Text("important")
      .highlighted()
  }
}
