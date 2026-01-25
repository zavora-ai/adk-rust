// Quick test of A2UI converter
const a2uiMessage = {
  components: [
    {
      id: "root",
      component: {
        Column: {
          children: [
            {
              id: "title",
              component: {
                Text: {
                  text: { literalString: "Welcome!" },
                  variant: "h1"
                }
              }
            },
            {
              id: "desc",
              component: {
                Text: {
                  text: { literalString: "This is A2UI v0.9" },
                  variant: "body"
                }
              }
            }
          ],
          spacing: 16
        }
      }
    }
  ]
};

console.log("A2UI Input:");
console.log(JSON.stringify(a2uiMessage, null, 2));
console.log("\nExpected flat output:");
console.log("- Column with 2 Text children");
console.log("- Title: 'Welcome!' (h1)");
console.log("- Description: 'This is A2UI v0.9' (body)");
