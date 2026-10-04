{
  inputs.nested.url = "path:./nested";
  outputs = { self, nested }: {
    value = "dep";
    selfValue = self.value;
    nestedValue = nested.value;
  };
}
