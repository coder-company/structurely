import React from "react";

export function Child() {
  return <span>ready</span>;
}

export class App extends React.Component {
  update() {
    this.setState({ ready: true });
  }

  render() {
    return <Child />;
  }
}
