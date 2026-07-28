@Component
export struct Counter {
  @State count: number = 0;

  increment() {
    this.count++;
  }

  build() {
    Column() {
      Text(`${this.count}`)
      Button("Increment").onClick(this.increment)
    }
  }
}
