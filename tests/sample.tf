# Terraform sample
resource "null_resource" "example" {
  triggers = {
    value = "hello"
  }
}
