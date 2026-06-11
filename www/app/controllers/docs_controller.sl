# Documentation pages for soli-sfu. Static content lives in the views; each
# action just names the page for the shared nav's active state.

class DocsController < Controller
    static {
        this.layout = "application"
    }

    def overview
        @title = "Overview"
        @page  = "overview"
    end

    def install
        @title = "Installation"
        @page  = "install"
    end

    def architecture
        @title = "Architecture"
        @page  = "architecture"
    end

    def api
        @title = "Control API"
        @page  = "api"
    end

    def tokens
        @title = "Tokens"
        @page  = "tokens"
    end

    def client
        @title = "Client contract"
        @page  = "client"
    end

    def bonfire
        @title = "Bonfire integration"
        @page  = "bonfire"
    end

    def ops
        @title = "Operations"
        @page  = "ops"
    end
end
