import { createApp } from "vue";
import App from "./App.vue";
import { router } from "./router";
import { configureNavigation } from "./services/operations";
import "./style.css";

configureNavigation(router);
createApp(App).use(router).mount("#app");
